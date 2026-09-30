"""Explain package readiness from one observed, versioned snapshot."""

import hashlib
import json
import re
from datetime import datetime, timezone


class ContractError(ValueError):
    """Malformed workflow input with a stable machine-readable code."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


POINTER = ("source_type", "project", "object_type", "object_id")
IDENTITY = (*POINTER, "revision")
FACETS = ("tracker", "git", "ledger", "ownership")
LEDGER_STATES = {
    "READY": "open",
    "BLOCKED": "active",
    "CLAIMED": "active",
    "IN_PROGRESS": "active",
    "DELIVERED": "active",
    "ACCEPTED": "accepted",
    "REJECTED": "rejected",
}


def _object(value, fields, code="INVALID_INPUT"):
    if not isinstance(value, dict) or any(key not in value for key in fields):
        raise ContractError(code, f"Expected object with {', '.join(fields)}")
    return value


def _text(value, code="INVALID_INPUT"):
    if not isinstance(value, str) or not value.strip():
        raise ContractError(code, "Expected nonempty text")
    return value


def _list(value):
    if not isinstance(value, list):
        raise ContractError("INVALID_INPUT", "Expected list")
    return value


def _pointer(value):
    obj = _object(value, POINTER)
    return tuple(_text(obj[key]) for key in POINTER)


def _identity(value):
    obj = _object(value, IDENTITY)
    return (*_pointer(obj), _text(obj["revision"]))


def _time(value):
    try:
        result = datetime.fromisoformat(_text(value).replace("Z", "+00:00"))
        if result.tzinfo is None:
            raise ValueError("missing timezone")
        return result
    except ValueError as exc:
        raise ContractError("INVALID_TIME", str(exc)) from exc


def _canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def parse_acceptance_body(body: str) -> dict:
    """Parse the sole fenced acceptance record, allowing outer blank space."""
    if not isinstance(body, str):
        raise ContractError("INVALID_ACCEPTANCE", "Acceptance body must be text")
    match = re.fullmatch(
        r"\s*```workflow-acceptance[ \t]*\n(.*?)\n```[ \t]*\s*", body, re.DOTALL
    )
    if match is None:
        raise ContractError(
            "INVALID_ACCEPTANCE", "Expected one workflow-acceptance block"
        )

    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ContractError("DUPLICATE_JSON_KEY", f"Duplicate JSON key: {key}")
            result[key] = value
        return result

    try:
        marker = json.loads(match.group(1), object_pairs_hook=unique)
    except json.JSONDecodeError as exc:
        raise ContractError("INVALID_ACCEPTANCE", str(exc)) from exc
    if not isinstance(marker, dict):
        raise ContractError("INVALID_ACCEPTANCE", "Acceptance record must be an object")
    return marker


def _path(value):
    value = _text(value)
    if (
        value.startswith("/")
        or value.endswith("/")
        or any(part in ("", ".", "..") for part in value.split("/"))
    ):
        raise ContractError(
            "INVALID_PATH", "Owned path must be normalized and repository relative"
        )
    return value


def paths_overlap(left: str, right: str) -> bool:
    """Whether two normalized repository paths reserve any common files."""
    return left == right or left.startswith(right + "/") or right.startswith(left + "/")


def _profile(profile):
    _object(
        profile,
        (
            "schema_version",
            "profile_id",
            "projects",
            "coordination_root",
            "max_source_age_seconds",
            "checks",
            "capabilities",
            "protected_resources",
        ),
    )
    if type(profile["schema_version"]) is not int or profile["schema_version"] != 1:
        raise ContractError("UNKNOWN_VERSION", "Profile schema_version must be 1")
    _text(profile["profile_id"])
    _text(profile["coordination_root"])
    if (
        type(profile["max_source_age_seconds"]) is not int
        or profile["max_source_age_seconds"] <= 0
    ):
        raise ContractError(
            "INVALID_PROFILE", "max_source_age_seconds must be positive"
        )
    if not isinstance(profile["projects"], dict) or not profile["projects"]:
        raise ContractError("INVALID_PROFILE", "Projects must be a nonempty object")
    if not isinstance(profile["checks"], dict):
        raise ContractError("INVALID_PROFILE", "Checks must be an object")
    for check_id, check in profile["checks"].items():
        _text(check_id)
        _object(check, ("command", "cwd", "environment"))
        _text(check["command"])
        _text(check["cwd"])
        if check.get("evidence_scope", "local") not in (
            "local",
            "hosted",
            "integrated",
            "operational",
        ):
            raise ContractError("INVALID_PROFILE", "Unknown check evidence scope")
        if not isinstance(check["environment"], dict) or any(
            not isinstance(key, str) or not isinstance(value, str)
            for key, value in check["environment"].items()
        ):
            raise ContractError(
                "INVALID_PROFILE", "Check environment must be string map"
            )
    if not _list(profile["capabilities"]):
        raise ContractError("INVALID_PROFILE", "Agent capabilities are required")
    for capability in profile["capabilities"]:
        _object(capability, ("model", "efforts"))
        _text(capability["model"])
        if not _list(capability["efforts"]) or any(
            effort not in ("low", "medium", "high", "xhigh", "max", "ultra")
            for effort in capability["efforts"]
        ):
            raise ContractError("INVALID_PROFILE", "Unknown or absent effort")
    for resource in _list(profile["protected_resources"]):
        _text(resource)
    for project in profile["projects"].values():
        _object(project, ("tracker", "git", "ledger", "lifecycle_sources"))
        _object(
            project["tracker"],
            ("source_type", "project", "dependency_source", "acceptance_actors"),
        )
        if project["tracker"]["dependency_source"] not in ("workflow-json", "native"):
            raise ContractError("INVALID_PROFILE", "Unknown dependency source")
        for key in ("source_type", "project"):
            _text(project["tracker"][key])
        for actor in _list(project["tracker"]["acceptance_actors"]):
            _text(actor)
        _object(
            project["git"],
            ("source_type", "project", "default_branch", "release_branch"),
        )
        for value in project["git"].values():
            _text(value)
        if project["git"]["release_branch"] == project["git"]["default_branch"]:
            raise ContractError(
                "INVALID_PROFILE", "Release branch must differ from default branch"
            )
        _object(project["ledger"], ("packages_path", "active_path", "reader"))
        for value in project["ledger"].values():
            _text(value)
        if not isinstance(project["lifecycle_sources"], dict):
            raise ContractError("INVALID_PROFILE", "Lifecycle sources must be a map")
        for prefix, source in project["lifecycle_sources"].items():
            _text(prefix)
            if source not in ("ledger", "tracker-acceptance"):
                raise ContractError("INVALID_PROFILE", "Unknown lifecycle source")


def _metadata(metadata, checks):
    fields = (
        "schema_version",
        "key",
        "outcome",
        "kind",
        "release",
        "parent",
        "requires",
        "contracts",
        "owned_paths",
        "resources",
        "check_ids",
        "criteria",
        "acceptance_owner",
        "risk",
        "uncertainty",
        "impact",
    )
    _object(metadata, fields)
    if type(metadata["schema_version"]) is not int or metadata["schema_version"] != 1:
        raise ContractError("UNKNOWN_VERSION", "Package schema_version must be 1")
    for key in ("key", "outcome", "release", "acceptance_owner"):
        _text(metadata[key])
    if metadata["kind"] not in (
        "investigation",
        "contract",
        "implementation",
        "validation",
    ):
        raise ContractError("INVALID_KIND", "Unknown package kind")
    if metadata["risk"] not in ("low", "medium", "high") or metadata[
        "uncertainty"
    ] not in ("known", "investigation-needed"):
        raise ContractError("INVALID_ROUTE", "Unknown risk or uncertainty")
    if metadata["parent"] is not None:
        _pointer(metadata["parent"])
    for key in (
        "requires",
        "contracts",
        "owned_paths",
        "resources",
        "check_ids",
        "criteria",
    ):
        _list(metadata[key])
    for path in metadata["owned_paths"]:
        _path(path)
    for resource in metadata["resources"]:
        _text(resource)
    if not metadata["criteria"]:
        raise ContractError("MISSING_CRITERIA", "At least one criterion is required")
    for item in metadata["criteria"]:
        _object(item, ("id", "expected", "evidence_scope"))
        _text(item["id"])
        _text(item["expected"])
        if item["evidence_scope"] not in (
            "local",
            "hosted",
            "integrated",
            "operational",
        ):
            raise ContractError("INVALID_CRITERION", "Unknown evidence scope")
    for edge in metadata["requires"] + metadata["contracts"]:
        _object(edge, ("target", "accepted_revision", "artifact"))
        _pointer(edge["target"])
        _text(edge["accepted_revision"])
        _text(edge["artifact"])
    for check in metadata["check_ids"]:
        if not isinstance(check, str) or check not in checks:
            raise ContractError(
                "UNREVIEWED_CHECK", "Check ID is absent from reviewed profile"
            )
    impact = metadata["impact"]
    if impact != "unknown":
        _object(impact, ("paths", "check_ids"))
        _list(impact["paths"])
        _list(impact["check_ids"])
        for path in impact["paths"]:
            _path(path)
        if any(check not in checks for check in impact["check_ids"]):
            raise ContractError(
                "UNREVIEWED_CHECK", "Impact check ID is absent from reviewed profile"
            )


def _project(profile, identity):
    for key, binding in profile["projects"].items():
        tracker = binding["tracker"]
        if (tracker["source_type"], tracker["project"]) == identity[:2]:
            return key, binding
    return None, None


def _acceptance(package, binding):
    lifecycle = package["lifecycle"]
    if lifecycle["state"] not in ("accepted", "rejected"):
        return True
    receipt = lifecycle.get("acceptance_receipt")
    if lifecycle.get("tracker_state") != "closed" or not isinstance(receipt, dict):
        return False
    if (
        receipt.get("author_login") != package["metadata"]["acceptance_owner"]
        or receipt.get("author_login") not in binding["tracker"]["acceptance_actors"]
    ):
        return False
    try:
        receipt_identity = _identity(receipt["identity"])
        package_identity = _identity(package["identity"])
        if (
            receipt_identity[:2] != package_identity[:2]
            or receipt_identity[2] != "issue_comment"
            or _pointer(receipt["parent"]) != package_identity[:4]
        ):
            return False
        marker = parse_acceptance_body(receipt["body"])
        return (
            marker.get("schema_version") == 1
            and _pointer(marker["target"]) == _identity(package["identity"])[:4]
            and marker.get("accepted_revision") == package["identity"]["revision"]
            and marker.get("status") == lifecycle["state"]
            and all(
                _text(marker.get(key))
                for key in ("artifact", "candidate_sha", "base_sha")
            )
        )
    except (KeyError, TypeError, ValueError, ContractError, json.JSONDecodeError):
        return False


def explain(profile, snapshot, target, now=None):
    """Return a source-qualified, deterministic readiness explanation."""
    _profile(profile)
    _object(
        snapshot,
        (
            "schema_version",
            "profile_id",
            "observed_at",
            "sources",
            "packages",
            "git",
            "ledger",
            "references",
            "claims",
            "receipts",
            "evidence",
            "reviews",
        ),
    )
    if type(snapshot["schema_version"]) is not int or snapshot["schema_version"] != 1:
        raise ContractError("UNKNOWN_VERSION", "Snapshot schema_version must be 1")
    if snapshot["profile_id"] != profile["profile_id"]:
        raise ContractError("PROFILE_MISMATCH", "Snapshot belongs to another profile")
    now = now or datetime.now(timezone.utc)
    if now.tzinfo is None:
        raise ContractError("INVALID_TIME", "Query time needs a timezone")
    target_key = _pointer(target)
    packages = {}
    for package in _list(snapshot["packages"]):
        _object(package, ("identity", "metadata", "lifecycle"))
        identity = _identity(package["identity"])
        if identity[:4] in packages:
            raise ContractError("DUPLICATE_IDENTITY", "Duplicate package identity")
        _metadata(package["metadata"], profile["checks"])
        _object(package["lifecycle"], ("state", "source", "acceptance_receipt"))
        if package["lifecycle"]["state"] not in (
            "open",
            "active",
            "accepted",
            "rejected",
            "unknown",
        ):
            raise ContractError("INVALID_LIFECYCLE", "Unknown lifecycle state")
        packages[identity[:4]] = package
    observations = {}
    for source in _list(snapshot["sources"]):
        _object(source, ("project", "facet", "observed_at", "complete", "errors"))
        key = (_text(source["project"]), source["facet"])
        if (
            key in observations
            or source["facet"] not in FACETS
            or type(source["complete"]) is not bool
        ):
            raise ContractError(
                "INVALID_SOURCE", "Duplicate or malformed source observation"
            )
        _list(source["errors"])
        observations[key] = source
    references = {}
    for reference in _list(snapshot["references"]):
        identity = _identity(_object(reference, ("identity",))["identity"])
        if identity[:4] in references:
            raise ContractError("DUPLICATE_IDENTITY", "Duplicate reference identity")
        references[identity[:4]] = identity[4]
    for key in ("git", "claims", "receipts", "evidence", "reviews"):
        _list(snapshot[key])
    _object(snapshot["ledger"], ("packages", "active"))
    _list(snapshot["ledger"]["packages"])
    ledger_records = {}
    for record in snapshot["ledger"]["packages"]:
        _object(record, ("state",))
        _text(record["state"])
        record_key = record.get("key", record.get("id"))
        _text(record_key)
        ledger_records.setdefault((record.get("project"), record_key), []).append(
            record
        )

    unknown, blocked, dependencies, seen, visiting = [], [], [], set(), set()

    def check_sources(key):
        project, binding = _project(profile, key)
        if binding is None:
            unknown.append({"code": "UNBOUND_PROJECT", "target": list(key)})
            return None
        for facet in FACETS:
            source = observations.get((project, facet))
            if (
                source is None
                or not source["complete"]
                or source["errors"]
                or abs((now - _time(source["observed_at"])).total_seconds())
                > profile["max_source_age_seconds"]
            ):
                unknown.append(
                    {"code": "SOURCE_UNAVAILABLE", "project": project, "facet": facet}
                )
        return binding

    def visit(key):
        if key in visiting:
            blocked.append({"code": "CYCLE", "target": list(key)})
            return
        if key in seen:
            return
        visiting.add(key)
        binding = check_sources(key)
        package = packages.get(key)
        if package is None:
            unknown.append({"code": "MISSING_PACKAGE", "target": list(key)})
        else:
            if binding is not None:
                if (
                    key == target_key
                    and package["metadata"]["release"]
                    != binding["git"]["release_branch"]
                ):
                    blocked.append(
                        {"code": "RELEASE_BINDING_MISMATCH", "target": list(key)}
                    )
                project_key, _ = _project(profile, key)
                source_kind = next(
                    (
                        v
                        for prefix, v in binding["lifecycle_sources"].items()
                        if package["metadata"]["key"].startswith(prefix)
                    ),
                    None,
                )
                if source_kind is None:
                    unknown.append({"code": "LIFECYCLE_UNBOUND", "target": list(key)})
                elif source_kind == "ledger":
                    records = ledger_records.get(
                        (project_key, package["metadata"]["key"]), []
                    )
                    if len(records) != 1:
                        unknown.append(
                            {
                                "code": "LEDGER_RECORD_MISSING_OR_AMBIGUOUS",
                                "target": list(key),
                            }
                        )
                    else:
                        authoritative = LEDGER_STATES.get(records[0]["state"])
                        if (
                            authoritative is None
                            or authoritative != package["lifecycle"]["state"]
                        ):
                            unknown.append(
                                {
                                    "code": "LEDGER_LIFECYCLE_MISMATCH",
                                    "target": list(key),
                                }
                            )
                        if key != target_key and authoritative != "accepted":
                            blocked.append(
                                {
                                    "code": "PREREQUISITE_NOT_ACCEPTED",
                                    "target": list(key),
                                    "state": authoritative or "unknown",
                                }
                            )
                elif package["lifecycle"]["state"] == "unknown":
                    unknown.append({"code": "LIFECYCLE_UNKNOWN", "target": list(key)})
                elif source_kind == "tracker-acceptance" and not _acceptance(
                    package, binding
                ):
                    unknown.append(
                        {"code": "ACCEPTANCE_UNVERIFIED", "target": list(key)}
                    )
                if key == target_key and package["lifecycle"]["state"] == "rejected":
                    blocked.append({"code": "TARGET_REJECTED", "target": list(key)})
                if key == target_key and package["lifecycle"]["state"] == "accepted":
                    blocked.append(
                        {"code": "TARGET_ALREADY_ACCEPTED", "target": list(key)}
                    )
                if binding["tracker"]["dependency_source"] != "workflow-json":
                    unknown.append(
                        {"code": "UNSUPPORTED_DEPENDENCY_SOURCE", "target": list(key)}
                    )
                for edge in package["metadata"]["contracts"]:
                    ref = _pointer(edge["target"])
                    if references.get(ref) != edge["accepted_revision"]:
                        unknown.append(
                            {
                                "code": "CONTRACT_REVISION",
                                "target": list(ref),
                                "accepted_revision": edge["accepted_revision"],
                            }
                        )
                for edge in package["metadata"]["requires"]:
                    dependency = _pointer(edge["target"])
                    dependencies.append(
                        {
                            "target": edge["target"],
                            "accepted_revision": edge["accepted_revision"],
                            "state": packages.get(dependency, {})
                            .get("lifecycle", {})
                            .get("state", "unknown"),
                        }
                    )
                    prerequisite = packages.get(dependency)
                    if (
                        prerequisite
                        and prerequisite["identity"]["revision"]
                        != edge["accepted_revision"]
                    ):
                        unknown.append(
                            {
                                "code": "DEPENDENCY_REVISION",
                                "target": list(dependency),
                                "accepted_revision": edge["accepted_revision"],
                            }
                        )
                    visit(dependency)
                    if (
                        prerequisite
                        and prerequisite["lifecycle"]["state"] != "accepted"
                    ):
                        blocked.append(
                            {
                                "code": "PREREQUISITE_NOT_ACCEPTED",
                                "target": list(dependency),
                                "state": prerequisite["lifecycle"]["state"],
                            }
                        )
        visiting.remove(key)
        seen.add(key)

    visit(target_key)
    active_pointer = snapshot["ledger"]["active"]
    if active_pointer:
        active_key = (
            active_pointer.get("workPackageId")
            if isinstance(active_pointer, dict)
            else None
        )
        if not any(
            record.get("key", record.get("id")) == active_key
            for record in snapshot["ledger"]["packages"]
        ):
            unknown.append(
                {"code": "LEGACY_POINTER_UNRECONCILED", "pointer": active_pointer}
            )
    target_package = packages.get(target_key)
    target_project = (
        _project(profile, _identity(target_package["identity"]))[0]
        if target_package
        else None
    )
    for record in snapshot["ledger"]["packages"]:
        if str(record["state"]).upper() not in (
            "CLAIMED",
            "IN_PROGRESS",
            "DELIVERED",
            "BLOCKED",
            "ACTIVE",
        ):
            continue
        owner_key = record.get("key", record.get("id"))
        try:
            resources = [_text(value) for value in _list(record.get("resources"))]
            paths = [_path(value) for value in _list(record.get("owned_paths"))]
            if (
                not resources
                or not paths
                or any(
                    value not in profile["protected_resources"] for value in resources
                )
            ):
                raise ContractError(
                    "LEGACY_SCOPE_UNKNOWN", "Incomplete legacy ownership scope"
                )
        except ContractError:
            reason = {"code": "LEGACY_OWNERSHIP_UNKNOWN", "package": owner_key}
            if record.get("project") in (None, target_project):
                blocked.append(reason)
            else:
                unknown.append(reason)
            continue
        if target_package is None:
            continue
        metadata = target_package["metadata"]
        if set(resources).intersection(metadata["resources"]):
            blocked.append({"code": "LEGACY_RESOURCE_CONFLICT", "package": owner_key})
        if record.get("project") in (None, target_project):
            if owner_key == metadata["key"]:
                blocked.append({"code": "LEGACY_ACTIVE_PACKAGE", "package": owner_key})
            elif any(
                paths_overlap(left.casefold(), right.casefold())
                for left in paths
                for right in metadata["owned_paths"]
            ):
                blocked.append({"code": "LEGACY_PATH_CONFLICT", "package": owner_key})
    for claim in snapshot["claims"]:
        _object(claim, ("attempt_id", "state"))
        _text(claim["attempt_id"])
        if claim["state"] not in ("intent", "claimed", "uncertain", "recorded"):
            raise ContractError("INVALID_CLAIM_STATE", "Unknown claim state")
        if claim.get("state") not in ("intent", "claimed", "uncertain"):
            continue
        try:
            claim_package = _pointer(claim["package"])
            paths = [_path(path) for path in _list(claim["owned_paths"])]
            resources = [_text(resource) for resource in _list(claim["resources"])]
        except (KeyError, ContractError):
            unknown.append(
                {"code": "CLAIM_SCOPE_UNKNOWN", "attempt_id": claim.get("attempt_id")}
            )
            continue
        if target_package and (
            claim_package == target_key
            or set(resources).intersection(target_package["metadata"]["resources"])
            or (
                claim_package[:2] == target_key[:2]
                and any(
                    paths_overlap(left, right)
                    for left in paths
                    for right in target_package["metadata"]["owned_paths"]
                )
            )
        ):
            blocked.append(
                {"code": "ACTIVE_CLAIM", "attempt_id": claim.get("attempt_id")}
            )
    status = "UNKNOWN" if unknown else "BLOCKED" if blocked else "READY"
    subject = packages.get(target_key)
    kind = subject["metadata"]["kind"] if subject else None
    next_step = (
        "investigate"
        if status == "UNKNOWN"
        else "wait"
        if status == "BLOCKED"
        else "investigate"
        if subject and subject["metadata"]["uncertainty"] == "investigation-needed"
        else {
            "contract": "contract",
            "implementation": "implement",
            "validation": "verify",
            "investigation": "investigate",
        }.get(kind, "investigate")
    )
    semantic = {
        "profile": profile,
        "packages": sorted(
            packages.values(), key=lambda item: _identity(item["identity"])
        ),
        "references": sorted(
            snapshot["references"], key=lambda item: _identity(item["identity"])
        ),
        "git": sorted(snapshot["git"], key=lambda item: item["project"]),
        "sources": sorted(
            (
                {
                    "project": item["project"],
                    "facet": item["facet"],
                    "complete": item["complete"],
                    "errors": sorted(item["errors"], key=_canonical),
                }
                for item in snapshot["sources"]
            ),
            key=lambda item: (item["project"], item["facet"]),
        ),
        "ledger": {
            "packages": sorted(snapshot["ledger"]["packages"], key=_canonical),
            "active": snapshot["ledger"]["active"],
        },
        "claims": sorted(
            snapshot["claims"], key=lambda item: item.get("attempt_id", "")
        ),
    }
    fingerprint = hashlib.sha256(_canonical(semantic).encode()).hexdigest()
    return {
        "schema_version": 1,
        "ok": True,
        "status": status,
        "next_step": next_step,
        "target": target,
        "dependencies": sorted(dependencies, key=lambda item: _pointer(item["target"])),
        "reasons": unknown + blocked,
        "sources": sorted(
            snapshot["sources"], key=lambda item: (item["project"], item["facet"])
        ),
        "fingerprint": fingerprint,
    }
