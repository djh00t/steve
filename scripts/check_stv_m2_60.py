"""Execute the proposed STV-M2-60 principal contract examples.

This is a reference fixture, not a test of Steve's future principal runtime.
"""

from __future__ import annotations

import json
import sqlite3
from contextlib import closing
from pathlib import Path
from uuid import UUID


CONTRACT = Path(__file__).resolve().parents[1] / "docs/contracts/stv-m2-60.md"
ATTRIBUTION_KEYS = {"organisation_id", "user_id", "client_id"}


def require(condition: bool, message: str) -> None:
    """Keep fixture checks active when Python optimization is enabled."""
    if not condition:
        raise AssertionError(message)


def examples() -> dict[str, object]:
    """Load the JSON fixture embedded in the contract artifact."""
    source = CONTRACT.read_text(encoding="utf-8")
    start = source.index("### Contract examples and executable reference fixture")
    block = source[start:].split("```json\n", 1)[1].split("\n```", 1)[0]
    return json.loads(block)


def attribution(record: dict[str, object]) -> tuple[str, str, str] | None:
    """Apply the proposal's all-or-null attribution wire rule."""
    value = record.get("attribution")
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != ATTRIBUTION_KEYS:
        raise ValueError("attribution must contain exactly three IDs")
    ids = []
    for key in ("organisation_id", "user_id", "client_id"):
        raw = value[key]
        if not isinstance(raw, str):
            raise ValueError(f"{key} must be a UUID string")
        parsed = UUID(raw)
        if parsed.version != 7 or str(parsed) != raw:
            raise ValueError(f"{key} must be a canonical UUIDv7")
        ids.append(raw)
    return tuple(ids)


def must_reject(record: dict[str, object]) -> None:
    """Fail if invalid attribution passes the reference validator."""
    try:
        attribution(record)
    except ValueError:
        return
    raise AssertionError(f"accepted invalid attribution: {record!r}")


def must_reject_sql(
    db: sqlite3.Connection,
    sql: str,
    values: tuple[object, ...],
    expected_error: str,
) -> None:
    """Fail if a proposed SQLite constraint does not reject the write."""
    try:
        db.execute(sql, values)
    except sqlite3.IntegrityError as exc:
        require(expected_error in str(exc), str(exc))
        return
    raise AssertionError(f"accepted invalid SQL write: {sql}")


def check_wire(data: dict[str, object]) -> tuple[str, str, str]:
    """Exercise complete, legacy, partial, malformed and unknown wire cases."""
    request = data["complete_request"]
    attempt = data["complete_attempt"]
    require(isinstance(request, dict) and isinstance(attempt, dict), "missing records")
    require("attribution" in request and "attribution" in attempt, "missing member")
    captured = attribution(request)
    require(captured is not None and attribution(attempt) == captured, "tuple drift")
    for key in ("legacy_request_attribution_absent", "legacy_request_attribution_null"):
        record = data[key]
        require(isinstance(record, dict) and attribution(record) is None, key)
    partial = data["reject_partial_attribution"]
    require(isinstance(partial, dict), "missing partial case")
    must_reject(partial)
    must_reject({"attribution": {"organisation_id": captured[0]}})
    must_reject({"attribution": dict(request["attribution"], extra="unknown")})
    must_reject({"attribution": dict(request["attribution"], client_id="not-a-uuid")})
    return captured


def check_relationships(
    data: dict[str, object], captured: tuple[str, str, str]
) -> None:
    """Exercise proposed parent links, inactive ancestry and delete protection."""
    with closing(sqlite3.connect(":memory:")) as db:
        db.execute("PRAGMA foreign_keys = ON")
        db.executescript(
            """
            CREATE TABLE organisations (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL CHECK (name <> ''),
                created_at TEXT NOT NULL,
                inactive_at TEXT
            );
            CREATE TABLE users (
                id TEXT PRIMARY KEY NOT NULL,
                organisation_id TEXT NOT NULL
                    REFERENCES organisations(id) ON DELETE RESTRICT,
                name TEXT NOT NULL CHECK (name <> ''),
                created_at TEXT NOT NULL,
                inactive_at TEXT
            );
            CREATE TABLE clients (
                id TEXT PRIMARY KEY NOT NULL,
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
                name TEXT NOT NULL CHECK (name <> ''),
                created_at TEXT NOT NULL,
                inactive_at TEXT
            );
            """
        )
        stamp = "2026-01-01T00:00:00Z"
        db.execute(
            "INSERT INTO organisations VALUES (?, ?, ?, NULL)",
            (captured[0], "Org", stamp),
        )
        db.execute(
            "INSERT INTO users VALUES (?, ?, ?, ?, NULL)",
            (captured[1], captured[0], "User", stamp),
        )
        db.execute(
            "INSERT INTO clients VALUES (?, ?, ?, ?, NULL)",
            (captured[2], captured[1], "Client", stamp),
        )
        db.commit()

        active = """SELECT o.id, u.id, c.id FROM clients c
                    JOIN users u ON u.id = c.user_id
                    JOIN organisations o ON o.id = u.organisation_id
                    WHERE c.id = ? AND o.inactive_at IS NULL
                    AND u.inactive_at IS NULL AND c.inactive_at IS NULL"""
        require(
            db.execute(active, (captured[2],)).fetchone() == captured,
            "active chain",
        )

        missing = data["reject_user_with_missing_organisation"]
        require(isinstance(missing, dict), "missing parent case")
        row = missing["user_row"]
        require(isinstance(row, dict), "missing user row")
        must_reject_sql(
            db,
            "INSERT INTO users VALUES (?, ?, ?, ?, ?)",
            (
                row["id"],
                row["organisation_id"],
                row["name"],
                row["created_at"],
                row["inactive_at"],
            ),
            "FOREIGN KEY constraint failed",
        )
        must_reject_sql(
            db,
            "INSERT INTO clients VALUES (?, ?, ?, ?, NULL)",
            ("00000000-0000-7000-8000-000000000010", row["id"], "Orphan", stamp),
            "FOREIGN KEY constraint failed",
        )
        must_reject_sql(
            db,
            "INSERT INTO users VALUES (?, ?, ?, ?, NULL)",
            ("00000000-0000-7000-8000-000000000011", captured[0], "", stamp),
            "CHECK constraint failed",
        )

        db.execute(
            "UPDATE organisations SET inactive_at = ? WHERE id = ?",
            (stamp, captured[0]),
        )
        require(db.execute(active, (captured[2],)).fetchone() is None, "inactive chain")
        require(
            db.execute("SELECT id FROM clients WHERE id = ?", (captured[2],)).fetchone()
            is not None,
            "lost historical Client",
        )
        must_reject_sql(
            db,
            "DELETE FROM organisations WHERE id = ?",
            (captured[0],),
            "FOREIGN KEY constraint failed",
        )
        for table in ("organisations", "users", "clients"):
            require(
                db.execute(f"SELECT count(*) FROM {table}").fetchone() == (1,),
                table,
            )


def main() -> None:
    """Run the contract's executable reference fixture."""
    data = examples()
    captured = check_wire(data)
    check_relationships(data, captured)
    print("STV-M2-60 contract fixture: passed (reference semantics only)")


if __name__ == "__main__":
    main()
