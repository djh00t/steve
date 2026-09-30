"""Read GitHub issue, Git and existing ledger facts without mutating them."""

import hashlib
import json
import subprocess
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote

from .graph import (
    LEDGER_STATES,
    ContractError,
    _path,
    _pointer,
    _profile,
    parse_acceptance_body,
)


def _unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ContractError("DUPLICATE_JSON_KEY", f"Duplicate JSON key: {key}")
        result[key] = value
    return result


def _fetch_github(path):
    result = subprocess.run(
        ["gh", "api", path], capture_output=True, text=True, check=False
    )
    if result.returncode:
        raise OSError(f"GitHub API request failed: {path}")
    return json.loads(result.stdout, object_pairs_hook=_unique_pairs)


def _metadata(body):
    start = "```workflow-json\n"
    if not isinstance(body, str) or body.count(start) != 1:
        raise ContractError(
            "MISSING_WORKFLOW_BLOCK", "Issue needs exactly one workflow-json block"
        )
    value = body.split(start, 1)[1].split("\n```", 1)
    if len(value) != 2:
        raise ContractError("MALFORMED_WORKFLOW_BLOCK", "Unclosed workflow-json block")
    try:
        return json.loads(value[0], object_pairs_hook=_unique_pairs)
    except json.JSONDecodeError as exc:
        raise ContractError("MALFORMED_WORKFLOW_BLOCK", str(exc)) from exc


def _json_file(path):
    return json.loads(
        Path(path).expanduser().read_text(), object_pairs_hook=_unique_pairs
    )


def read_github(
    profile: dict, target: dict, fetch_json=None, now: datetime | None = None
) -> dict:
    """Observe explicit GitHub dependencies and local legacy ownership.

    `fetch_json(path)` may be injected for tests; the default uses authenticated
    `gh api`. Provider failures remain structured source errors in the snapshot.
    """
    _profile(profile)
    target_key = _pointer(target)
    if target_key[0] != "github" or target_key[2] != "issue":
        raise ContractError(
            "UNSUPPORTED_SOURCE", "GitHub reader requires an issue pointer"
        )
    now = now or datetime.now(timezone.utc)
    stamp = now.isoformat().replace("+00:00", "Z")
    fetch = fetch_json or _fetch_github
    bindings = {}
    for key, binding in profile["projects"].items():
        tracker = binding["tracker"]
        bindings[(tracker["source_type"], tracker["project"])] = (key, binding)
    if target_key[:2] not in bindings:
        raise ContractError("UNBOUND_PROJECT", "Target has no profile binding")
    if bindings[target_key[:2]][1]["tracker"]["dependency_source"] != "workflow-json":
        raise ContractError(
            "UNSUPPORTED_DEPENDENCY_SOURCE",
            "Native GitHub dependencies are not implemented",
        )
    observations = {}
    packages = {}
    references = {}
    git = []
    ledger = {"packages": [], "active": None}
    pending = [target_key]
    visited = set()

    def source(project, facet):
        return observations.setdefault(
            (project, facet),
            {
                "project": project,
                "facet": facet,
                "observed_at": stamp,
                "complete": True,
                "errors": [],
            },
        )

    def error(project, facet, code, message):
        record = source(project, facet)
        record["complete"] = False
        record["errors"].append({"code": code, "message": message})

    def pages(path):
        values = []
        for number in range(1, 101):
            page = fetch(f"{path}?per_page=100&page={number}")
            if not isinstance(page, list):
                raise ContractError(
                    "INVALID_PROVIDER_RESPONSE", "Expected a page array"
                )
            values.extend(page)
            if len(page) < 100:
                return values
        raise ContractError("PAGINATION_LIMIT", "More than 100 pages")

    def observe_project(key, binding):
        if (key, "git") in observations:
            return
        for facet in ("tracker", "git", "ledger", "ownership"):
            source(key, facet)
        repo = binding["git"]["project"]
        branch = binding["git"]["release_branch"]
        heads = {}
        for name in (binding["git"]["default_branch"], branch):
            try:
                reply = fetch(f"repos/{repo}/branches/{quote(name, safe='')}")
                heads[name] = reply["commit"]["sha"]
            except (OSError, KeyError, TypeError, ValueError, ContractError) as exc:
                error(key, "git", "GIT_READ_FAILED", str(exc))
        git.append(
            {
                "project": key,
                "default_head": heads.get(binding["git"]["default_branch"]),
                "release_branch": branch,
                "release_head": heads.get(branch),
                "release_pr": None,
            }
        )
        try:
            records = _json_file(binding["ledger"]["packages_path"])
            if not isinstance(records, dict) or not isinstance(
                records.get("packages"), list
            ):
                raise ContractError("INVALID_LEDGER", "Expected packages array")
            for row in records["packages"]:
                if not isinstance(row, dict):
                    raise ContractError("INVALID_LEDGER", "Expected package object")
                ledger["packages"].append(dict(row, project=key))
            active = _json_file(binding["ledger"]["active_path"])
            if active:
                if ledger["active"] is None:
                    ledger["active"] = active
                else:
                    ledger["packages"].append(
                        {
                            "key": active.get("workPackageId"),
                            "state": "active",
                            "project": key,
                        }
                    )
        except (
            OSError,
            UnicodeError,
            ValueError,
            KeyError,
            TypeError,
            ContractError,
        ) as exc:
            error(key, "ledger", "LEDGER_READ_FAILED", str(exc))
            error(key, "ownership", "OWNERSHIP_READ_FAILED", str(exc))

    while pending:
        item = pending.pop()
        if item in visited:
            continue
        visited.add(item)
        provider = bindings.get(item[:2])
        if provider is None:
            continue
        project, binding = provider
        if binding["tracker"]["dependency_source"] != "workflow-json":
            error(
                project,
                "tracker",
                "UNSUPPORTED_DEPENDENCY_SOURCE",
                "Native GitHub dependencies are not implemented",
            )
            continue
        observe_project(project, binding)
        repo = binding["tracker"]["project"]
        try:
            issue = fetch(f"repos/{repo}/issues/{quote(item[3], safe='')}")
            if (
                not isinstance(issue, dict)
                or str(issue.get("number")) != item[3]
                or issue.get("state") not in ("open", "closed")
            ):
                raise ContractError(
                    "INVALID_PROVIDER_RESPONSE", "Malformed issue response"
                )
            body = issue.get("body")
            metadata = _metadata(body)
            revision = hashlib.sha256(body.encode()).hexdigest()
            source_kind = next(
                (
                    kind
                    for prefix, kind in binding["lifecycle_sources"].items()
                    if metadata["key"].startswith(prefix)
                ),
                None,
            )
            receipt = None
            disposition = None
            if source_kind == "ledger":
                matches = [
                    row
                    for row in ledger["packages"]
                    if row.get("project") == project
                    and row.get("key", row.get("id")) == metadata["key"]
                ]
                if len(matches) == 1:
                    disposition = LEDGER_STATES.get(matches[0].get("state"))
            elif source_kind == "tracker-acceptance":
                comments = pages(
                    f"repos/{repo}/issues/{quote(item[3], safe='')}/comments"
                )
                receipt = next(
                    (
                        {
                            "identity": {
                                "source_type": "github",
                                "project": repo,
                                "object_type": "issue_comment",
                                "object_id": str(comment["id"]),
                                "revision": comment["updated_at"],
                            },
                            "author_login": comment["user"]["login"],
                            "parent": {
                                "source_type": "github",
                                "project": repo,
                                "object_type": "issue",
                                "object_id": item[3],
                            },
                            "body": comment["body"],
                        }
                        for comment in reversed(comments)
                        if isinstance(comment, dict)
                        and isinstance(comment.get("body"), str)
                        and comment["body"]
                        .lstrip()
                        .startswith("```workflow-acceptance")
                    ),
                    None,
                )
                if receipt:
                    try:
                        marker = parse_acceptance_body(receipt["body"])
                        if marker.get("status") in ("accepted", "rejected"):
                            disposition = marker["status"]
                    except (ValueError, ContractError):
                        pass
            lifecycle = {
                "state": disposition
                or (
                    "open"
                    if source_kind == "tracker-acceptance" and issue["state"] == "open"
                    else "unknown"
                ),
                "source": {
                    "source_type": "github",
                    "project": repo,
                    "object_type": "issue",
                    "object_id": item[3],
                },
                "acceptance_receipt": receipt,
                "tracker_state": issue["state"],
            }
            packages[item] = {
                "identity": {
                    "source_type": "github",
                    "project": repo,
                    "object_type": "issue",
                    "object_id": item[3],
                    "revision": revision,
                },
                "metadata": metadata,
                "lifecycle": lifecycle,
            }
            for edge in metadata.get("contracts", []):
                try:
                    pointer = _pointer(edge["target"])
                    if pointer in references:
                        continue
                    if pointer[0] != "github" or pointer[2] != "file":
                        raise ContractError(
                            "UNSUPPORTED_REFERENCE", "Expected GitHub file reference"
                        )
                    ref_provider = bindings.get(pointer[:2])
                    if ref_provider is None:
                        raise ContractError(
                            "UNBOUND_REFERENCE", "Contract project is unbound"
                        )
                    ref_project, ref_binding = ref_provider
                    observe_project(ref_project, ref_binding)
                    path = _path(pointer[3])
                    branch = ref_binding["git"]["release_branch"]
                    reply = fetch(
                        f"repos/{pointer[1]}/contents/{quote(path, safe='/')}?ref={quote(branch, safe='')}"
                    )
                    if (
                        not isinstance(reply, dict)
                        or reply.get("type") != "file"
                        or not isinstance(reply.get("sha"), str)
                    ):
                        raise ContractError(
                            "INVALID_REFERENCE", "Expected file content SHA"
                        )
                    references[pointer] = {
                        "identity": {**edge["target"], "revision": reply["sha"]}
                    }
                except (OSError, ValueError, KeyError, TypeError, ContractError) as exc:
                    error(project, "git", "CONTRACT_READ_FAILED", str(exc))
            for edge in metadata.get("requires", []):
                pointer = _pointer(edge["target"])
                if pointer[:2] in bindings:
                    pending.append(pointer)
                else:
                    error(
                        project,
                        "tracker",
                        "UNBOUND_DEPENDENCY",
                        "Explicit dependency lacks project binding",
                    )
        except (
            OSError,
            UnicodeError,
            ValueError,
            KeyError,
            TypeError,
            ContractError,
        ) as exc:
            code = exc.code if isinstance(exc, ContractError) else "TRACKER_READ_FAILED"
            error(project, "tracker", code, str(exc))

    return {
        "schema_version": 1,
        "profile_id": profile["profile_id"],
        "observed_at": stamp,
        "sources": list(observations.values()),
        "packages": list(packages.values()),
        "git": git,
        "ledger": ledger,
        "references": list(references.values()),
        "claims": [],
        "receipts": [],
        "evidence": [],
        "reviews": [],
    }
