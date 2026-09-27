# STV-V1-12 — Native supervisor/coordinator requirements

**Purpose:** M8 requirements input for #350. This inventory records existing evidence and open boundaries; it is not a selected design, approved contract, implementation, or dispatch-ready child list. The V1 issue ID and parent #71 remain for tracking; M8 is the required product phase.

## Operator need and boundary

A managed native update must let an operator stage a versioned candidate while the current Steve worker continues serving, then move traffic only after candidate checks pass. The coordinator boundary is local to a managed native installation. A remote controller does not own the remote Steve process. Container/Kubernetes replacement belongs to the runtime orchestrator; Steve must expose compatible readiness/drain semantics without applying the native coordinator to those deployments.

## Current source evidence

- `src/main.rs::serve` connects and migrates the DB, opens object storage, starts `DeferredQueues`, then calls `server::run` in the same process. `main.rs` has no supervisor mode.
- `src/server.rs::run` binds the configured inference and management TCP listeners, builds one app state, marks that process ready after both listeners bind, and serves both listeners in that process. `serve_until_drained` handles Ctrl-C/SIGTERM for that worker.
- `src/lifecycle.rs::Lifecycle` tracks one worker's `starting/ready/draining/stopped` phase, in-flight guards, and wait-for-zero. It does not represent two generations, endpoint ownership, cutover, or route-back.
- `src/config.rs::ServerConfig` provides inference/management bind addresses and one drain timeout (default 60 seconds); there is no service identity, package receipt, child-process control, or handoff setting.
- `src/deferred.rs::DeferredQueues` starts per-process accounting/history/telemetry workers and replays the accounting journal at startup. `src/storage/db.rs::Database::migrate` runs before serving and records schema versions; no mixed-version compatibility gate is present here.
- Existing real-process tests are `tests/e2e.rs::drain_active_stream` (ready becomes `not_ready`, liveness remains available, held SSE completes, then SIGTERM exits within the bound) and Unix `tests/e2e_process_shutdown.rs::process_sigterm_exits_cleanly`. These are ordinary single-worker drain/shutdown evidence, not coordinator evidence; this discovery did not run them.

## Existing-spec minimum and negative cases

`docs/specs/2026-09-26-steve-gateway.md` §18 (lines 539–600) requires a native supervisor/coordinator that owns or mediates the stable endpoint and worker lifecycle. Minimum sequence: stage a signed candidate at a versioned path; verify trust and compatibility; start it with the same external configuration; wait for migration/compatibility checks and readiness; direct new work to it; mark the old worker draining; finish active streams/tasks and critical accounting/audit work or hand it to the replacement/durable journal; stop the old worker after zero active work or the configured maximum drain deadline; route back automatically if the candidate fails before cutover completes. Mixed-version migrations must be expand/contract; destructive cleanup waits until old workers exit.

- Candidate signature, compatibility, or readiness failure must leave the current worker serving; do not replace it in place or stop it before cutover.
- A candidate failure before cutover completion must route back automatically. Manual reinstall is not the normal rollback path.
- Drain deadline behavior with active streams or unfinished accounting/audit work needs an explicit failure/recovery rule. Current `serve_until_drained` logs remaining in-flight work at timeout and shuts down that worker; it does not hand state to another worker.
- A migration that cannot safely operate with both versions must block the update before migration. #424 records this existing-spec minimum; #428 may discover later migration extensions and is not a prerequisite for it.

## Open inputs and current owners

- **Stable endpoint/listener ownership and transfer:** the spec says the coordinator owns or mediates it, but transport/socket transfer is unspecified. #350 must retain this decision; do not assume socket activation, a proxy, or another mechanism.
- **Child control, readiness, and authentication:** existing `/health/ready`, `/health/live`, and `/api/v1/system/status` describe a worker; they do not define supervisor-to-child control or trust. Consume #337 deployment-action policy and management auth/TLS contracts (#106/#109) after their existing acceptance gates; keep the child boundary unresolved until those inputs are exact.
- **Crash and drain cutoff:** #89 covers ordinary process drain. #350 must cover candidate/old-worker crashes around readiness, cutover, route-back, and the maximum drain deadline; ordinary drain does not qualify those transitions. The spec requires route-back before cutover completes; ownership/recovery for failure after that boundary remains unresolved.
- **Mixed-version compatibility:** #424 owns the minimum gate above; #428 retains later migration discovery. Startup migration currently runs before serving, so compatibility and failure-before-migration behavior need proof.
- **Signing and installed identity:** #336 owns native source/trust policy (proposal acceptance remains pending); #341 owns service/package identity and receipt; #423 records release/install requirements. The #336 proposal records actual Apple signer identity/credentials as unavailable; do not guess them.
- **Accounting/audit overlap and recovery:** `src/deferred.rs` replays a process-local journal and spills failed/full queue writes; `src/storage/db.rs` inserts background events idempotently by event ID. Neither defines concurrent old/new journal ownership or a cross-worker drain handoff. Reuse #84–#87 for overflow semantics, framing, reconciliation and replay proof; #350 must gate overlap on that evidence.

## Candidate decomposition order for #350

These are proposed file boundaries and process scenarios for #350 to refine, not created/approved work packages. New paths and scenarios below do not exist and are not runnable today. Producer implementation/qualification leaves own their real checks as done-means; only downstream consumers wait for accepted producer output PRs and delivered proof.

| Order | Candidate ownership for #350 to refine | Proposed process evidence (not runnable today) |
| --- | --- | --- |
| 1. Managed child lifecycle | Existing `src/main.rs`; candidate new `src/supervisor.rs`; extend `tests/support/process.rs`; candidate new `tests/e2e_native_update.rs`. | Start old worker and candidate under managed mode; prove candidate staging/readiness failure leaves old worker and stable endpoint available. Exact listener/control mechanism remains open. |
| 2. Readiness, cutover, drain, route-back | Candidate `src/supervisor.rs`; existing `src/server.rs` and `src/lifecycle.rs`; same candidate process test. | Hold a real SSE stream on old worker, cut over only after candidate readiness, complete old stream, then fail a candidate before cutover completion and observe automatic route-back. Also prove bounded failure handling at drain deadline. |
| 3. Compatibility and durable work | Existing `src/storage/db.rs` and `src/deferred.rs`; same candidate process test, with fixtures owned by the producer. | Run old/new workers against a compatible schema and held accounting work; prove no duplicate/lost event across drain/restart. An incompatible migration must block before migration. Depends on #424 minimum and #84–#87 recovery inputs. |

#350 owns conversion of these requirements into bounded, dependency-ordered implementation leaves with exact file owners and runnable tests. All leaves require their accepted input contracts. Producer leaves must not wait for their own test or qualification output; update/UI action consumers wait for the corresponding accepted producer PR and proof. No runtime behavior, architecture choice, or issue IDs are created by this discovery.
