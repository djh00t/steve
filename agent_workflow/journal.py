"""Conditional SQLite claims and sourced native operation receipts."""

import json
import sqlite3
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath

from .graph import ContractError, explain, paths_overlap
from .routing import select_route

DB_NAME = "journal.sqlite3"
ACTIVE = "released_at IS NULL"
TERMINAL = {"completed", "not-dispatched", "cancelled"}
ROUTES = {"inline", "delegated", "investigation"}
STEPS = {
    "investigate",
    "contract",
    "implement",
    "diagnose",
    "repair",
    "verify",
    "review",
    "integrate",
    "handoff",
}
STAGES = {"dispatch", "child-pr", "integration", "handoff"}
RESULTS = {"pending", "running", "failed", "unknown", *TERMINAL}


def _json(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True)


def _text(value, name):
    if not isinstance(value, str) or not value.strip() or value != value.strip():
        raise ContractError("INVALID_INPUT", f"{name} must be nonempty trimmed text")
    return value


def _identity(value, revision=True):
    fields = ("source_type", "project", "object_type", "object_id")
    if revision:
        fields += ("revision",)
    if not isinstance(value, dict) or set(value) != set(fields):
        raise ContractError(
            "INVALID_IDENTITY", "Expected exact source-qualified identity"
        )
    return tuple(_text(value[key], key) for key in fields)


def _path(value):
    value = _text(value, "owned_path")
    if (
        value.startswith("/")
        or "\\" in value
        or "\x00" in value
        or PurePosixPath(value).as_posix() != value
        or any(part in ("", ".", "..") for part in value.split("/"))
    ):
        raise ContractError("INVALID_PATH", "Owned path must be canonical and relative")
    return value


def _connect(root, writable):
    path = Path(root).expanduser().resolve() / DB_NAME
    if not writable and not path.exists():
        return None
    if writable:
        path.parent.mkdir(parents=True, exist_ok=True)
        db = sqlite3.connect(path, timeout=30, isolation_level=None)
        db.execute("PRAGMA foreign_keys = ON")
        db.executescript(
            """CREATE TABLE IF NOT EXISTS attempts (
                attempt_id TEXT PRIMARY KEY,
                package TEXT NOT NULL,
                owner TEXT NOT NULL,
                authority_reference TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                base_sha TEXT NOT NULL,
                candidate_sha TEXT NOT NULL,
                route TEXT NOT NULL,
                step TEXT NOT NULL,
                owned_paths TEXT NOT NULL,
                resources TEXT NOT NULL,
                state TEXT NOT NULL,
                heartbeat TEXT NOT NULL,
                released_at TEXT
            );
            CREATE TABLE IF NOT EXISTS receipts (
                receipt_id TEXT PRIMARY KEY,
                attempt_id TEXT NOT NULL REFERENCES attempts(attempt_id),
                body TEXT NOT NULL
            );"""
        )
    else:
        db = sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    return db


def _claims(db):
    claims = []
    for row in db.execute(f"SELECT * FROM attempts WHERE {ACTIVE} ORDER BY attempt_id"):
        package = json.loads(row["package"])
        claims.append(
            {
                "attempt_id": row["attempt_id"],
                "package": {
                    key: package[key]
                    for key in ("source_type", "project", "object_type", "object_id")
                },
                "state": "uncertain" if row["state"] == "intent" else "claimed",
                "owned_paths": json.loads(row["owned_paths"]),
                "resources": json.loads(row["resources"]),
                "owner": row["owner"],
                "heartbeat": row["heartbeat"],
            }
        )
    return claims


def read_claims(root):
    """Read graph-compatible active claims; absent roots create no database."""
    db = _connect(root, False)
    if db is None:
        return []
    try:
        return _claims(db)
    finally:
        db.close()


def _receipts(db, attempt_id=None):
    return [
        {
            "attempt_id": row["attempt_id"],
            "package": json.loads(row["package"]),
            "fingerprint": row["fingerprint"],
            "base_sha": row["base_sha"],
            "candidate_sha": row["candidate_sha"],
            **json.loads(row["body"]),
        }
        for row in db.execute(
            "SELECT receipts.body, attempts.attempt_id, attempts.package, attempts.fingerprint, "
            "attempts.base_sha, attempts.candidate_sha FROM receipts "
            "JOIN attempts USING (attempt_id) WHERE (? IS NULL OR attempts.attempt_id = ?) "
            "ORDER BY receipts.rowid",
            (attempt_id, attempt_id),
        )
    ]


def read_receipts(root, attempt_id=None):
    """Read ordered receipts with original fingerprint and revisions; no DB creation."""
    if attempt_id is not None:
        _text(attempt_id, "attempt_id")
    db = _connect(root, False)
    if db is None:
        return []
    try:
        return _receipts(db, attempt_id)
    finally:
        db.close()


def claim(profile, target, request, refresh, now=None):
    """Reserve a fresh READY package under BEGIN IMMEDIATE.

    ``refresh()`` reads authoritative sources and returns a snapshot. Its claims
    are replaced with current journal claims inside the write transaction.
    ``request`` supplies attempt_id, owner, authority_reference,
    expected_fingerprint, route, step, base_sha and candidate_sha.
    """
    if not isinstance(request, dict) or not callable(refresh):
        raise ContractError("INVALID_INPUT", "Claim needs request and source refresh")
    for key in (
        "attempt_id",
        "owner",
        "authority_reference",
        "expected_fingerprint",
        "base_sha",
        "candidate_sha",
    ):
        _text(request.get(key), key)
    if request.get("route") not in ROUTES or request.get("step") not in STEPS:
        raise ContractError("INVALID_INPUT", "Unknown route or step")
    pointer = _identity(target, False)
    db = _connect(profile["coordination_root"], True)
    try:
        db.execute("BEGIN IMMEDIATE")
        if db.execute(
            "SELECT 1 FROM attempts WHERE attempt_id = ?", (request["attempt_id"],)
        ).fetchone():
            raise ContractError(
                "ATTEMPT_CONFLICT",
                "Attempt ID already exists; reconcile it before retry",
            )
        snapshot = refresh()
        if not isinstance(snapshot, dict):
            raise ContractError(
                "INVALID_INPUT", "Source refresh must return a snapshot"
            )
        snapshot = {**snapshot, "claims": _claims(db)}
        answer = explain(profile, snapshot, target, now)
        package = next(
            (
                item
                for item in snapshot["packages"]
                if _identity(item["identity"])[:4] == pointer
            ),
            None,
        )
        if package is None:
            raise ContractError("NOT_READY", "Target package is not observed")
        metadata = package["metadata"]
        paths = [_path(path) for path in metadata["owned_paths"]]
        resources = [_text(resource, "resource") for resource in metadata["resources"]]
        protected = profile["protected_resources"]
        if any(resource not in protected for resource in resources) or len(
            resources
        ) != len(set(resources)):
            raise ContractError(
                "INVALID_RESOURCE", "Resource must name one canonical profile resource"
            )
        if len(paths) != len(set(paths)):
            raise ContractError("INVALID_PATH", "Duplicate owned path")
        for prior in snapshot["claims"]:
            same_package = _identity(prior["package"], False) == pointer
            same_project = (
                tuple(prior["package"][key] for key in ("source_type", "project"))
                == pointer[:2]
            )
            path_conflict = same_project and any(
                paths_overlap(left.casefold(), right.casefold())
                for left in paths
                for right in prior["owned_paths"]
            )
            if (
                same_package
                or path_conflict
                or set(resources).intersection(prior["resources"])
            ):
                raise ContractError(
                    "CLAIM_CONFLICT",
                    f"Active attempt {prior['attempt_id']} reserves this scope",
                )
        if answer["fingerprint"] != request["expected_fingerprint"]:
            raise ContractError(
                "STALE_FINGERPRINT", "Sources or active claims changed since explain"
            )
        if answer["status"] != "READY":
            raise ContractError("NOT_READY", f"Target is {answer['status']}")
        prior_receipts = [
            receipt
            for receipt in _receipts(db)
            if _identity(receipt["package"])[:4] == pointer
            and receipt["fingerprint"] == answer["fingerprint"]
        ]
        observations = [
            {
                **receipt["evidence"],
                "fingerprint": receipt["fingerprint"],
                "result": receipt["evidence"]["result"]
                if receipt["result"] == "completed"
                else "unknown",
            }
            for receipt in prior_receipts
        ]
        route = select_route(
            profile,
            package,
            answer["fingerprint"],
            observations,
            candidate_sha=request["candidate_sha"],
            base_sha=request["base_sha"],
        )
        if request["route"] != route["route"] or request["step"] != route["next_step"]:
            raise ContractError("ROUTE_MISMATCH", "Claim route or step is not current")
        if route["unavailable_reason"]:
            raise ContractError("ROUTE_UNAVAILABLE", route["unavailable_reason"])
        project = next(
            (
                name
                for name, binding in profile["projects"].items()
                if (binding["tracker"]["source_type"], binding["tracker"]["project"])
                == pointer[:2]
            ),
            None,
        )
        git = next(
            (item for item in snapshot["git"] if item["project"] == project), None
        )
        latest_candidate = next(
            (
                item["evidence"]
                for item in reversed(prior_receipts)
                if item["result"] == "completed"
                and item["evidence"]["result"] == "passed"
            ),
            None,
        )
        expected_base = (
            latest_candidate["base_sha"]
            if latest_candidate
            else git["release_head"]
            if git
            else None
        )
        expected_candidate = (
            latest_candidate["candidate_sha"]
            if latest_candidate
            else git["release_head"]
            if git
            else None
        )
        if git is None or (request["base_sha"], request["candidate_sha"]) != (
            expected_base,
            expected_candidate,
        ):
            raise ContractError(
                "REVISION_MISMATCH",
                "Claim base and candidate must match observed release head or sourced receipt",
            )
        stamp = (now or datetime.now(timezone.utc)).isoformat()
        db.execute(
            "INSERT INTO attempts VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
            (
                request["attempt_id"],
                _json(package["identity"]),
                request["owner"],
                request["authority_reference"],
                answer["fingerprint"],
                request["base_sha"],
                request["candidate_sha"],
                request["route"],
                request["step"],
                _json(paths),
                _json(resources),
                "intent",
                stamp,
            ),
        )
        db.commit()
        return {
            "schema_version": 1,
            "ok": True,
            "attempt_id": request["attempt_id"],
            "state": "intent",
            "package": package["identity"],
            "fingerprint": answer["fingerprint"],
            "base_sha": request["base_sha"],
            "candidate_sha": request["candidate_sha"],
            "owned_paths": paths,
            "resources": resources,
        }
    except Exception:
        db.rollback()
        raise
    finally:
        db.close()


def record(profile, request):
    """Persist one sourced native receipt; terminal reconciliation releases scope.

    ``request`` supplies attempt_id, owner, package_revision, base_sha,
    candidate_sha and receipt. Receipt has receipt_id, stage, external_object
    (exact native identity), result, observed_at and evidence with step,
    result (passed/failed/unknown), actual candidate_sha, base_sha, artifact.
    """
    if not isinstance(request, dict):
        raise ContractError("INVALID_INPUT", "Receipt request must be an object")
    for key in ("attempt_id", "owner", "package_revision", "base_sha", "candidate_sha"):
        _text(request.get(key), key)
    receipt = request.get("receipt")
    if not isinstance(receipt, dict) or set(receipt) != {
        "receipt_id",
        "stage",
        "external_object",
        "result",
        "observed_at",
        "evidence",
    }:
        raise ContractError(
            "INVALID_RECEIPT", "Receipt must contain sourced native result"
        )
    _text(receipt["receipt_id"], "receipt_id")
    _identity(receipt["external_object"])
    if (
        receipt["stage"] not in STAGES
        or receipt["result"] not in RESULTS
        or not isinstance(receipt["evidence"], dict)
    ):
        raise ContractError(
            "INVALID_RECEIPT", "Unknown receipt stage, result or evidence"
        )
    evidence = receipt["evidence"]
    if set(evidence) != {"step", "result", "candidate_sha", "base_sha", "artifact"}:
        raise ContractError(
            "INVALID_RECEIPT",
            "Evidence needs step, result, candidate, base and artifact",
        )
    if evidence["step"] not in STEPS or evidence["result"] not in {
        "passed",
        "failed",
        "unknown",
    }:
        raise ContractError("INVALID_RECEIPT", "Unknown evidence step or result")
    if evidence["result"] == "passed" and receipt["result"] != "completed":
        raise ContractError(
            "INVALID_RECEIPT", "Only completed native work can pass a step"
        )
    for key in ("candidate_sha", "base_sha", "artifact"):
        _text(evidence[key], key)
    if evidence["base_sha"] != request["base_sha"]:
        raise ContractError(
            "REVISION_MISMATCH", "Evidence base differs from attempt base"
        )
    try:
        observed = datetime.fromisoformat(
            _text(receipt["observed_at"], "observed_at").replace("Z", "+00:00")
        )
        if observed.tzinfo is None:
            raise ValueError("missing timezone")
    except ValueError as exc:
        raise ContractError("INVALID_TIME", str(exc)) from exc
    root = profile["coordination_root"]
    if not (Path(root).expanduser().resolve() / DB_NAME).exists():
        raise ContractError("UNKNOWN_ATTEMPT", "No journal exists for this attempt")
    db = _connect(root, True)
    try:
        db.execute("BEGIN IMMEDIATE")
        attempt = db.execute(
            "SELECT * FROM attempts WHERE attempt_id = ?", (request["attempt_id"],)
        ).fetchone()
        if attempt is None:
            raise ContractError("UNKNOWN_ATTEMPT", "Attempt does not exist")
        if attempt["owner"] != request["owner"]:
            raise ContractError(
                "OWNER_MISMATCH", "Only the recorded owner can reconcile this attempt"
            )
        if (
            json.loads(attempt["package"])["revision"],
            attempt["base_sha"],
            attempt["candidate_sha"],
        ) != (
            request["package_revision"],
            request["base_sha"],
            request["candidate_sha"],
        ):
            raise ContractError(
                "REVISION_MISMATCH", "Receipt does not match original attempt revisions"
            )
        if receipt["evidence"]["step"] != attempt["step"]:
            raise ContractError(
                "STEP_MISMATCH", "Receipt step differs from claimed step"
            )
        body = _json(receipt)
        prior = db.execute(
            "SELECT attempt_id, body FROM receipts WHERE receipt_id = ?",
            (receipt["receipt_id"],),
        ).fetchone()
        if prior:
            if prior["attempt_id"] != request["attempt_id"] or prior["body"] != body:
                raise ContractError(
                    "RECEIPT_CONFLICT", "Receipt ID already has different content"
                )
            db.commit()
            return {
                "schema_version": 1,
                "ok": True,
                "attempt_id": request["attempt_id"],
                "receipt_id": receipt["receipt_id"],
                "state": "recorded",
                "released": attempt["released_at"] is not None,
            }
        if attempt["released_at"] is not None:
            raise ContractError(
                "ATTEMPT_CLOSED", "Released attempt cannot accept another receipt"
            )
        history = _receipts(db, request["attempt_id"])
        latest = max(
            (
                datetime.fromisoformat(attempt["heartbeat"]),
                *(
                    datetime.fromisoformat(item["observed_at"].replace("Z", "+00:00"))
                    for item in history
                ),
            )
        )
        if observed < latest:
            raise ContractError(
                "STALE_RECEIPT", "Receipt predates claim or prior observation"
            )
        if receipt["result"] == "not-dispatched" and history:
            raise ContractError(
                "RECONCILIATION_CONFLICT",
                "An earlier native observation contradicts not-dispatched",
            )
        if any(
            item["stage"] != receipt["stage"]
            or _identity(item["external_object"])[:4]
            != _identity(receipt["external_object"])[:4]
            for item in history
        ):
            raise ContractError(
                "IDENTITY_CONFLICT",
                "Receipt names a different native stage or object for this attempt",
            )
        db.execute(
            "INSERT INTO receipts VALUES (?, ?, ?)",
            (receipt["receipt_id"], request["attempt_id"], body),
        )
        released = receipt["result"] in {"cancelled", "not-dispatched"} or (
            receipt["result"] == "completed"
            and evidence["result"] in {"passed", "failed"}
        )
        if released:
            db.execute(
                "UPDATE attempts SET state = 'recorded', released_at = ? WHERE attempt_id = ?",
                (receipt["observed_at"], request["attempt_id"]),
            )
        else:
            db.execute(
                "UPDATE attempts SET state = 'recorded' WHERE attempt_id = ?",
                (request["attempt_id"],),
            )
        db.commit()
        return {
            "schema_version": 1,
            "ok": True,
            "attempt_id": request["attempt_id"],
            "receipt_id": receipt["receipt_id"],
            "state": "recorded",
            "released": released,
        }
    except Exception:
        db.rollback()
        raise
    finally:
        db.close()
