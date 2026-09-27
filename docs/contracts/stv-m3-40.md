# STV-M3-40 migration versioning and backend parity — proposal

**Status:** Proposed for review; not accepted and not an implementation contract.
**Evidence revision:** `6d635f546b66514ed96393803806e12015add4dc` (fresh origin/main); migration source matches reviewed `6b2694b`. No runtime results are claimed.
**Authority needed:** David/Cos review and explicit acceptance of an exact revision. No user approval or issue update is implied.
**Purpose:** Decision-only `DOC-REVIEW-STV-M3-40`; no runtime command is required today. Runtime consumers remain blocked until exact acceptance and updated self-contained briefs; each consumer must implement and qualify its own named test before delivery.

## Source evidence

- `src/storage/db.rs:124-205`: SQLite and PostgreSQL each create `steve_schema_migrations`, read `MAX(version)`, and apply only v1 (`m0_foundation`). The SQL is duplicated; SQLite uses `INTEGER`, PostgreSQL `BIGINT`; both record the same name and create `steve_background_events` in a transaction.
- `src/storage/db.rs:126-140,168-182`: ledger creation/version discovery are outside the migration transaction; `MAX` alone does not detect holes, name mismatches, or an unknown future version. `IF NOT EXISTS` does not verify an existing table's shape.
- Earliest tracked implementation `c7fe928` introduced ledger and foundation together. Known supported baseline is ledger v1 `m0_foundation`; no earlier tracked ledgerless schema was found. This does not prove no external/manual database exists.
- `src/main.rs:75-82,85-93`: `serve` and `doctor` call `db.migrate()` before server startup / database success. Migration or compatibility failure blocks startup/readiness.
- `docs/specs/2026-09-26-steve-gateway.md:350-359` requires SQLite and PostgreSQL; `:590,598` requires multi-replica rolling support and expand/contract compatibility.

## Proposed contract (for acceptance)

1. **Order/names:** integer `version` is the sole order key, starts at 1, is contiguous, and immutable. Preserve `(1, "m0_foundation")` exactly. Future entries increment by one and use stable descriptive names (for example `v0002_add_events_index`). Never edit/reuse a recorded version/name. Validate the complete ledger as a known contiguous prefix; reject gaps, name drift, or versions newer than this binary.
2. **One transaction per invocation:** acquire backend lock, create/read/validate ledger, apply every pending migration in order, and write every corresponding ledger row within one transaction. Commit once after the full pending suffix succeeds. A definite statement/transaction failure rolls back the invocation; no intermediate version prefix from this invocation is committed. After an uncertain commit result, discard that connection, reconnect, and validate ledger plus schema before deciding success/retry; never blindly rerun. A complete valid post-state is success; the unchanged valid pre-state permits a new attempt. Unavailable evidence or any other state fails startup with an indeterminate migration result, without repair or readiness.
3. **Fresh and v1 baseline:** an empty new DB is version 0 and receives v1, then pending versions. Existing supported databases have v1 ledger/name. Validate v1's expected schema shape even when the ledger says v1. No-ledger or empty-ledger startup is fresh only when no Steve application tables already exist. A pre-existing foundation table without its v1 ledger row is not an established supported baseline: fail visibly for operator repair rather than guessing adoption or dropping data. Validate backend-specific columns/types/nullability/primary keys for existing v1; preserve each backend's shipped shape. `CREATE TABLE IF NOT EXISTS` is not validation.
4. **Repeat/upgrade:** if the validated ledger is current, do no DDL. Otherwise apply the missing suffix within the single invocation transaction. Migration-specific validators own the expected post-migration schema manifest; ledger rows alone do not prove schema shape.
5. **Backend/concurrency parity:** same ordering, shape, commit, retry, and error contract on both backends. Begin SQLite with `BEGIN IMMEDIATE`; surface `SQLITE_BUSY` as visible startup contention, with no indefinite retry. In PostgreSQL, inside the transaction use nonblocking `pg_try_advisory_xact_lock` with the fixed application-owned key pair `(1398036037, 0)` (`STVE`, migration namespace); `false` is a visible startup contention error. Set SQLite busy timeout to 5,000 ms and PostgreSQL transaction-local `lock_timeout` to 5,000 ms ([PostgreSQL lock timeout semantics](https://www.postgresql.org/docs/17/runtime-config-client.html#GUC-LOCK-TIMEOUT)). Bound the entire invocation, including connection acquisition and outcome verification, to 30 seconds; expiry fails startup and discards the connection, without assuming a possibly committed transaction rolled back. There is no automatic in-process retry loop. Advisory try-lock only bounds cooperating migrators; the lock timeout and overall deadline also cover DDL contention. Both locks cover ledger read through final commit and release on transaction end. See [SQLite transaction docs](https://www.sqlite.org/lang_transaction.html) and [PostgreSQL explicit locking docs](https://www.postgresql.org/docs/17/explicit-locking.html).
6. **Readiness/compatibility:** preserve current pre-start ordering: migration/compatibility failure blocks readiness and `doctor` success. Expand/contract remains required; this proposal does not authorize incompatible rolling schemas.

## Baseline v1 manifest

Validate the ledger's physical shape before trusting its rows, and validate the application shape for the recorded version. Preserve the shipped backend definitions:

| Table | SQLite columns | PostgreSQL columns |
|---|---|---|
| `steve_schema_migrations` | `version INTEGER PRIMARY KEY`, `name TEXT NOT NULL`, `applied_at TEXT NOT NULL` | `version BIGINT PRIMARY KEY`, `name TEXT NOT NULL`, `applied_at TEXT NOT NULL` |
| `steve_background_events` | `id TEXT PRIMARY KEY`, `kind TEXT NOT NULL`, `payload TEXT NOT NULL`, `created_at TEXT NOT NULL` | Same SQL column declarations as SQLite |

As documented by [SQLite](https://www.sqlite.org/lang_createtable.html#the_primary_key), its existing `TEXT PRIMARY KEY` does not imply `NOT NULL`; PostgreSQL's does. SQLite's `INTEGER PRIMARY KEY` is the rowid alias. Validate those backend-specific catalog semantics, not identical raw nullability flags. These definitions have no column defaults or foreign keys. Reject missing/extra columns, wrong types, key/default/nullability drift, or extra data-changing constraints/triggers on these two tables; do not reject harmless additional non-unique indexes. Do not silently rewrite the legacy SQLite key or change stored event data. Later accepted migrations supply the expected manifest for their resulting version instead of requiring the old v1 shape forever.

## Future consumer qualification (proposed, not implemented)

Consumer-owned commands (both targets are absent today; neither is required for this decision-only review):

- SQLite: `cargo test --test e2e --all-features sqlite_migration_runner -- --exact --nocapture`
- PostgreSQL: `cargo test --test e2e --all-features postgres_migration_parity -- --exact --nocapture`

Qualify against real backend instances, baseline v1 only (feature DDL is out of scope): fresh v0 -> ordered v1/current ledger; rejection of pre-existing foundation tables without a v1 ledger row and incompatible shapes; existing v1 schema-shape validation; injected DB failure -> whole transaction rollback/no new ledger prefix -> successful retry; ambiguous commit -> reconnect/reread; repeated startup no-op; two concurrent starters -> one obtains lock and the other visibly fails contention without partial state. Issue 102 acceptance does not make these consumers ready; do not infer qualification from this proposal or from mocks.

## Review cases

**Accept if:** names/order are stable and version-driven; known v1 baseline is preserved and shape checked; all pending work and ledger rows share one transaction; failure/retry and uncertain commit behavior are explicit; lock contention is finite and visible; both backends share these semantics.

**Reject/revise if:** `MAX` accepts holes/name drift/future versions; DDL and ledger state can diverge; partial invocation prefixes can commit; a no-ledger DB is stamped current without exact shape validation; `IF NOT EXISTS` is treated as validation; either backend is claimed to have bounded DDL waits merely because the migration coordination lock is nonblocking; this document is represented as runtime or qualification evidence.

## Decision boundary

Review artifact only: no runtime, spec, or migration implementation changes. Documentation checks are recorded in the PR; no runtime qualification is claimed. Acceptance requires David/Cos to identify the exact accepted revision. After acceptance, re-size and update #103/#104 before dispatch; they own implementing and qualifying the commands above, and are not required to have already implemented their own tests merely to start. Exactly one scenario must run per command; zero tests is failure. Feature schema and mixed-version rolling-deploy mechanics are out of scope.
