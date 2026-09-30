"""Versioned JSON CLI; native dispatch stays in the active orchestrator chat."""

import argparse
import copy
import json
import sqlite3
import sys
import tempfile
from datetime import datetime
from pathlib import Path

from .graph import ContractError, _profile, _project, explain
from .routing import select_route
from .source import read_github


class Parser(argparse.ArgumentParser):
    def error(self, message):
        raise ContractError("INVALID_ARGUMENT", message)


def _unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ContractError("DUPLICATE_JSON_KEY", f"Duplicate JSON key: {key}")
        result[key] = value
    return result


def _read(path):
    try:
        return json.loads(Path(path).read_text(), object_pairs_hook=_unique_pairs)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ContractError("INVALID_JSON", str(exc)) from exc


def _rehearsal(profile, mode, root, writing):
    """Keep fixture mutations in an explicitly marked temporary directory."""
    marker = ".agent-workflow-fixture"
    live = Path(profile["coordination_root"]).expanduser().resolve()
    if mode == "live":
        if root or (live / marker).exists():
            raise ContractError(
                "INVALID_MODE", "Live input cannot use a rehearsal root"
            )
        return profile
    if not root:
        if writing:
            raise ContractError(
                "REHEARSAL_ROOT_REQUIRED", "Fixture writes require --rehearsal-root"
            )
        return profile
    path = Path(root).expanduser().resolve()
    allowed = {Path(tempfile.gettempdir()).resolve(), Path("/tmp").resolve()}
    if path == live or not any(
        path != base and path.is_relative_to(base) for base in allowed
    ):
        raise ContractError(
            "UNSAFE_REHEARSAL_ROOT",
            "Use a separate directory beneath the system temporary directory",
        )
    if path.exists() and not (path / marker).is_file() and any(path.iterdir()):
        raise ContractError(
            "UNSAFE_REHEARSAL_ROOT",
            "Nonempty unmarked directory is not a rehearsal journal",
        )
    if writing:
        path.mkdir(parents=True, exist_ok=True)
        (path / marker).touch(exist_ok=True)
    profile = copy.deepcopy(profile)
    profile["coordination_root"] = str(path)
    return profile


def _subject(snapshot, target):
    return next(
        item
        for item in snapshot["packages"]
        if all(item["identity"].get(key) == value for key, value in target.items())
    )


def _route(profile, snapshot, request, answer, observations):
    if answer["status"] == "READY":
        key = next(
            key
            for key, binding in profile["projects"].items()
            if binding["tracker"]["project"] == request["target"]["project"]
            and binding["tracker"]["source_type"] == request["target"]["source_type"]
        )
        base = next(
            item["release_head"] for item in snapshot["git"] if item["project"] == key
        )
        candidate = base
        for observation in observations:
            if (
                observation["fingerprint"] == answer["fingerprint"]
                and observation["result"] == "passed"
            ):
                candidate, base = observation["candidate_sha"], observation["base_sha"]
        if request.get("mode") == "live":
            if (
                request.get("candidate_sha", candidate) != candidate
                or request.get("base_sha", base) != base
            ):
                raise ContractError(
                    "REVISION_MISMATCH",
                    "Requested route revisions differ from current source or receipt",
                )
        else:
            candidate, base = (
                request.get("candidate_sha", candidate),
                request.get("base_sha", base),
            )
        answer.update(candidate_sha=candidate, base_sha=base)
        answer["execution"] = select_route(
            profile,
            _subject(snapshot, request["target"]),
            answer["fingerprint"],
            observations,
            candidate_sha=candidate,
            base_sha=base,
        )
        answer["next_step"] = answer["execution"]["next_step"]
    return answer


def _accepted_child(profile, pointer):
    """Require current accepted lifecycle, not merely eligibility for new work."""
    snapshot = read_github(profile, pointer)
    answer = explain(profile, snapshot, pointer)
    if answer["status"] != "BLOCKED" or {
        reason["code"] for reason in answer["reasons"]
    } != {"TARGET_ALREADY_ACCEPTED"}:
        raise ValueError("Child is not currently accepted with complete sources")
    references = [
        {
            "target": {
                key: value
                for key, value in item["identity"].items()
                if key != "revision"
            },
            "revision": item["identity"]["revision"],
        }
        for item in snapshot["packages"] + snapshot["references"]
    ]
    return _subject(snapshot, pointer), references


def main(argv=None):
    parser = Parser(prog="python -m agent_workflow")
    parser.add_argument(
        "operation", choices=("explain", "claim", "record", "release-check")
    )
    parser.add_argument("--profile", required=True)
    parser.add_argument("--input", required=True)
    parser.add_argument("--rehearsal-root")
    try:
        args = parser.parse_args(argv)
        profile_file, input_file = _read(args.profile), _read(args.input)
        if not isinstance(profile_file, dict) or not isinstance(input_file, dict):
            raise ContractError("INVALID_INPUT", "Profile and input must be objects")
        profile = profile_file.get("profile", profile_file)
        request = input_file.get("input", input_file)
        if (
            not isinstance(request, dict)
            or type(request.get("schema_version")) is not int
            or request["schema_version"] != 1
        ):
            raise ContractError("UNKNOWN_VERSION", "Input schema_version must be 1")
        _profile(profile)
        mode = request.get("mode")
        if mode not in ("live", "fixture"):
            raise ContractError("UNSUPPORTED_MODE", "Mode must be fixture or live")
        if mode == "live" and any(
            key in request for key in ("as_of", "snapshot", "observations")
        ):
            raise ContractError(
                "INVALID_INPUT",
                "Live input cannot supply time, snapshot or route observations",
            )
        writing = args.operation in ("claim", "record")
        profile = _rehearsal(profile, mode, args.rehearsal_root, writing)
        now = (
            datetime.fromisoformat(request["as_of"].replace("Z", "+00:00"))
            if mode == "fixture" and "as_of" in request
            else None
        )
        # Import only after validated input, keeping read-only errors deterministic.
        from .journal import claim, read_claims, read_receipts, record

        def refresh():
            snapshot = (
                copy.deepcopy(request["snapshot"])
                if mode == "fixture"
                else read_github(profile, request["target"])
            )
            if mode == "live" or args.rehearsal_root:
                snapshot["claims"] = snapshot["claims"] + read_claims(
                    profile["coordination_root"]
                )
            return snapshot

        if args.operation == "record":
            answer = record(profile, request)
        elif args.operation == "claim":
            answer = claim(profile, request["target"], request, refresh, now)
        else:
            snapshot = refresh()
            answer = explain(profile, snapshot, request["target"], now)
            if args.operation == "release-check":
                if answer["status"] == "READY":
                    from .admission import evaluate_admission
                    from .admission_source import refresh_admission

                    manifest = copy.deepcopy(request["manifest"])
                    subject = _subject(snapshot, request["target"])
                    # Replace reference observations, never their expected revisions.
                    manifest["references"] = [
                        {
                            "target": {
                                key: value
                                for key, value in item["identity"].items()
                                if key != "revision"
                            },
                            "revision": item["identity"]["revision"],
                        }
                        for item in snapshot["packages"] + snapshot["references"]
                    ]
                    if mode == "live":
                        _, binding = _project(
                            profile,
                            (
                                request["target"]["source_type"],
                                request["target"]["project"],
                            ),
                        )
                        manifest = refresh_admission(
                            binding["git"]["project"],
                            manifest,
                            read_package=lambda pointer: _accepted_child(
                                profile, pointer
                            ),
                        )
                    answer = evaluate_admission(profile, subject, manifest)
            else:
                observations = (
                    request.get("observations", []) if mode == "fixture" else []
                )
                if mode == "live" or args.rehearsal_root:
                    for receipt in read_receipts(
                        profile["coordination_root"], request.get("attempt_id")
                    ):
                        if all(
                            receipt["package"].get(key) == value
                            for key, value in request["target"].items()
                        ):
                            observation = dict(receipt["evidence"])
                            if receipt["result"] != "completed":
                                observation["result"] = "unknown"
                            observation["fingerprint"] = receipt["fingerprint"]
                            observations.append(observation)
                answer = _route(profile, snapshot, request, answer, observations)
        answer["mode"] = mode
        print(json.dumps(answer, sort_keys=True))
        return 0 if answer.get("ok", False) else 1
    except (
        ContractError,
        KeyError,
        TypeError,
        ValueError,
        OSError,
        StopIteration,
        sqlite3.Error,
    ) as exc:
        code = exc.code if isinstance(exc, ContractError) else "INVALID_INPUT"
        print(
            json.dumps(
                {
                    "schema_version": 1,
                    "ok": False,
                    "error": {"code": code, "message": str(exc), "details": {}},
                },
                sort_keys=True,
            )
        )
        return 2


if __name__ == "__main__":
    sys.exit(main())
