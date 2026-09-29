# M1 Release Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make PR #619 one user-operable M1 release candidate with complete #84/#85-#87 prerequisites and one final guided acceptance.

**Architecture:** Preserve `DeferredQueues::accounting(kind, payload)` for producers and reuse the real-process harness and SDK smoke. Isolate ownership, journal, incident, and drain state in `src/accounting.rs`; server admission/status consume its snapshot. A Python standard-library loopback page runs a fixed allowlist in guided or headless mode.

**Tech Stack:** Existing Rust/Axum/Tokio/SQLx stack, Rust standard-library file locking/sync available on the execution toolchain, pinned Python SDK test requirements, Python standard library.

**Spec:** `docs/m1-release-gate.md`

## Global Constraints

- Work in the held PR #619 after reconciling current `origin/main`; keep one integrated PR and Conventional Commits. Do not merge, deploy, or ask for intermediate operator acceptance.
- #95 nonstream emission is active in another worktree. Consume its reviewed commit; do not duplicate or overwrite its `src/server.rs` / `tests/e2e.rs` work. Serialize later shared edits after reconciliation.
- Preserve the public producer method `DeferredQueues::accounting(kind, payload)` and the exact `chat.attempt.terminal.v1` contract.
- Prefer existing capabilities and the standard library. Task 2 specifically justified direct `sha2` for evidence digests through Dependency Advisor; any further direct dependency requires that workflow. Make no unverified MSRV claim.
- Use private absolute local roots only. Missing, stale, corrupt, expired, or unsupported ownership evidence fails closed. Shared/network roots remain unsupported.
- M1 executable evidence covers process crash only. OS crash and power loss stay unknown. Retain the completed generation; retiring the only journal copy additionally requires SQLite WAL `synchronous=FULL` or PostgreSQL `fsync=on`, `synchronous_commit=on`, and `full_page_writes=on`, plus separately qualified storage durability for the claimed OS/power domain.
- Negative conflict/timeout/unknown scenarios pass only when the expected retained/unresolved behavior is observed.
- Run focused red/green checks and `make check` locally. Broad `make quality-gates` and release eligibility are exact-SHA hosted/final evidence.
- Tasks 2-7 are serial because they share accounting, process, server, or fixture seams.

## Review Focus

- Fresh provision versus missing/corrupt prior evidence; offline adoption must never become implicit startup migration and requires an operator-supplied assertion for the external stop/disable prerequisite.
- Revision, coverage tuple, and snapshot expiry races during overlapping replacement.
- SQLite/PostgreSQL full durability prerequisites versus conservative journal retention; M1 proves process crash only.
- EOF/error/drop races and nonstream retry cardinality across the combined #95/#96 result.
- Browser cross-origin/Host/token requests, output injection, subprocess timeout, and descendant cleanup.

---

### Task 1: Finish #485 and consumer handoffs

**Files:**
- Create: `docs/contracts/stv-m0-18.md`
- Modify: `docs/contracts/stv-m0-16.md`, `docs/contracts/stv-m0-17.md`, `docs/m1-acceptance.md`, `docs/contracts/README.md`, `docs/backlog-index.md`

**Interfaces:**
- Consumes: #483/#484 working baselines and live #85-#87/#94-#98 requirements.
- Produces: exact completion, retry-owner, reconciliation, disposition, shutdown, and qualification rules for Tasks 2-8.

- [ ] Specify `inserted`, `duplicate_identical`, `duplicate_conflict`, `failed`, and `unknown`; only the first two acknowledge DB completion. Leave retry timing to #86, which owns a configured per-operation timeout, total retry deadline, retry interval, exhaustion result, and startup/manual recovery trigger.
- [ ] Specify worker and journal barriers, successful and timed-out shutdown evidence, retirement ordering, verified reconciliation to `clear`, and operator disposition to `acknowledged`. The protected audit record requires incident revision, kind, actor, time, evidence reference, and coverage tuples; public health/status always redact actor/evidence reference to null. Verified records restore across restart.
- [ ] Copy #484's fresh provisioning, stable identity, revision/coverage, working-baseline polling/snapshot/lock bounds, missing-evidence failure, staging cleanup, and offline legacy adoption into the consumer handoff without inventing record families.
- [ ] Mark STV-M0-16 and STV-M0-17 as owner-selected working baselines whose final release acceptance remains the combined gate. Synchronize contract/backlog indexes and the live #84-#87, #94-#98, #37, #45, and #485 handoffs with the exact selected revisions, file ownership, prerequisites, and test targets before changing readiness.
- [ ] Assign `tests/e2e_accounting.rs` to #85-#87/#97/#485 and retain `tests/e2e.rs` for #95/#96; name every exact scenario command below in both repository docs and its owning issue. Documentation review must find every #84-#87/#485 requirement owned once and no passing runtime claim.
- [ ] Commit: `docs(m0): complete accounting recovery handoff`.

### Task 2: Provision and coordinate one replica root

**Files:**
- Create: `src/accounting.rs`, `tests/e2e_accounting.rs`
- Modify: `src/main.rs`, `src/config.rs`, `src/deferred.rs`, `tests/support/process.rs`, `Cargo.toml`, `config.toml`, `config.example.toml`, `Dockerfile`

**Interfaces:**
- Produces: `AccountingCoordinator::{provision,start,offer,snapshot,begin_shutdown}` behind the unchanged `DeferredQueues::accounting`; stable installation identity; revisioned incident evidence; locked generation state.

- [ ] Write failing `fresh_provision_and_missing_evidence_fail_closed`, `same_replica_replacement_preserves_accounting_ownership`, and `legacy_journal_adoption_is_offline_and_resumable` real-process scenarios. Each command is `cargo test --all-features --test e2e_accounting <name> -- --exact --nocapture` and must select one failing test.
- [ ] Implement explicit `steve accounting provision --root PATH` and offline `steve accounting adopt-legacy --source FILE --root PATH --maintenance-assertion FILE` commands. Require the operator-supplied assertion to name the source path, source host, assertion time, supervisor context, and claimed stop/disable action; retain its digest and fields in the migration manifest for audit. Treat it as unauthenticated: the protocol guard detects cooperating participants only, and Steve cannot establish that every uncooperative old writer stopped. Guard success and source hash/length recheck do not prove one cannot append later. Reject adoption without a complete assertion, but never label its presence verified stop evidence. Require `queues.accounting_journal` to name an absolute private root for serve; use `/var/lib/steve/accounting` in the container default/example and mount it persistently. Ordinary startup must never provision or adopt. Empty legacy input may complete vacuously; nonempty input remains staged/pending and ordinary startup fails closed until Task 3 replay acknowledgements complete.
- [ ] Under the coordination lock, reread and compare the monotonic revision; store coverage tuples `(generation_id, generation_state_revision, journal_evidence_digest)`; reject expired snapshots and any missing/corrupt/moved evidence. Use the #484 working-baseline freshness/lock bounds verbatim.
- [ ] Publish and sync a locked generation before readiness. A replacement may become ready beside a healthy busy predecessor but may not read, truncate, or retire its journal. Implement only #484's narrow empty unpublished-staging cleanup.
- [ ] Implement offline backup/hash/import staging and crash resume. The nonempty case retains source and backup without publishing a completion marker; the exact scenario passes by observing that pending/fail-closed boundary. Run the three exact scenarios; expected PASS.
- [ ] Commit: `feat(accounting): provision replica journal ownership`.

### Task 3: Frame, replay, and retain with backend durability

**Files:**
- Modify: `src/accounting.rs`, `src/storage/db.rs`, `src/config.rs`, `src/deferred.rs`, `tests/e2e_accounting.rs`, `tests/support/process.rs`

**Interfaces:**
- Produces: `InsertBackgroundEvent::{Inserted,DuplicateIdentical,DuplicateConflict}`, #86-owned retry policy, and safe generation completion/retention.

- [ ] Write failing `journal_partial_tail_recovery`, `accounting_reconciles_after_db_recovery`, `journal_partial_commit_replay`, and `postgres_replay_detects_conflicting_duplicate`. Use the exact command form from Task 2; PostgreSQL runs when `STEVE_TEST_POSTGRES_URL` is set and must fail, not skip, when configured but unavailable.
- [ ] Preserve complete newline frames and retain torn final bytes as unreconciled. Compare every duplicate's kind/payload/timestamp; a mismatch is conflict and remains retained.
- [ ] Complete any staged nonempty legacy adoption only after every imported record has a Task 3 replay acknowledgement; then sync the adoption marker, verify the source hash, move the source outside active discovery and sync its directory. Empty adoption needs no replay receipt.
- [ ] #86 adds explicit queue config for per-operation timeout, total retry deadline, and retry interval; validates positive/bounded relationships; latches exhaustion; retries on its bounded timer and explicit startup reconciliation. #485 does not select those values.
- [ ] Verify SQLite WAL accounting connections at `PRAGMA synchronous=FULL` and PostgreSQL at `fsync=on`, `synchronous_commit=on`, and `full_page_writes=on`. M1 still retains the completed generation because OS crash and power loss are unknown; future retirement requires separate storage qualification for that declared domain.
- [ ] Sync completion evidence and containing directory, but do not delete the only journal copy in M1. Run all four scenarios plus existing SQLite/PostgreSQL backend parity; expected PASS.
- [ ] Commit: `feat(accounting): retain and reconcile durable frames`.

### Task 4: Reconcile incidents and auditable disposition

**Files:**
- Modify: `src/accounting.rs`, `src/deferred.rs`, `src/main.rs`, `src/server.rs`, `tests/e2e_accounting.rs`, `tests/support/process.rs`

**Interfaces:**
- Produces: #483 incident admission/readiness/status, verified reconciliation, protected offline disposition, restart restoration, and public redaction.

- [ ] Write failing `accounting_incident_preserves_forwarding_and_restart_evidence`, `accounting_disposition_is_auditable_and_publicly_redacted`, and `accounting_conflict_recovery_is_offline_and_verified`. Run exact commands; current counters/gates should fail assertions.
- [ ] Serialize the incident latch before later inference capacity admission. Preserve the admitted response, return the contract's exact 503/headers afterward, and keep liveness/status reachable.
- [ ] Return to `clear` only when revision-checked coverage proves all retained/unknown work reconciled and no loss remains. Add offline `steve accounting acknowledge --root PATH --incident ID --revision N --kind KIND --evidence-ref REF` for confirmed loss/irreducible uncertainty; require zero pending replay. Derive actor from the effective UID/account on Unix or process-token SID/account on Windows; accept no actor option and ignore identity environment variables. Persist the protected record and redact actor/reference publicly. Add private read-only `steve accounting audit --root PATH --incident ID --revision N` for the full protected record.
- [ ] Add offline `steve accounting resolve-conflict --root PATH --incident ID --revision N --event-id ID --authoritative journal|database --evidence-ref REF`. Require all serving/replay processes stopped, verify both protected conflict sides/digests and stale revision before mutation, conditionally reconcile the selected authority, persist and sync the resolution record, then reread the final DB/evidence. Reject `acknowledge` for conflicts; failure preserves both sides and blocked/unreconciled state.
- [ ] Restart must restore `blocked`, `unreconciled`, or `acknowledged` truthfully; missing/unverifiable disposition restores `unreconciled`. Run both scenarios and `inference_saturation_keeps_management_live`; expected PASS.
- [ ] Commit: `feat(accounting): persist incident reconciliation and disposition`.

### Task 5: Use one shutdown sequence

**Files:**
- Modify: `src/accounting.rs`, `src/deferred.rs`, `src/lifecycle.rs`, `src/server.rs`, `tests/e2e_accounting.rs`, `tests/e2e_process_shutdown.rs`, `tests/support/process.rs`

**Interfaces:**
- Produces: one signal/management shutdown coordinator and persisted success/timeout evidence.

- [ ] Write failing `accounting_shutdown_barriers_complete` and `accounting_shutdown_timeout_survives_restart` scenarios for management drain and process signal. Assert management drain remains running/not-ready and signal exits. In the timeout case hold a writer past the deadline and prove a second real process cannot acquire or inspect that generation before the writer joins or the first process terminates.
- [ ] Calculate one absolute deadline once from `server.drain_timeout_seconds`. Before a final body/write can fail, sync generation state that forces unresolved recovery. Stop admission; finish bodies until the same deadline; cancel remaining bodies; close accounting producers; drain DB worker; pass journal flush/sync barrier with the remaining time; persist completion or leave the pre-persisted unresolved state. Release ownership only after every writer has stopped and joined. Successful management drain remains alive/not-ready and may release the completed generation. Management timeout remains alive with its lock and unresolved evidence while a writer is alive. Signal timeout retains ownership until process termination releases the OS lock. No stage receives a new timeout.
- [ ] Run both exact scenarios plus existing active-stream shutdown tests; expected PASS with no orphan process.
- [ ] Commit: `feat(lifecycle): drain accounting before process exit`.

### Task 6: Integrate #95 and emit terminal SSE accounting

**Files:**
- Modify after #95 reconciliation: `src/accounting.rs`, `src/deferred.rs`, `src/proxy/openai_chat.rs`, `tests/e2e.rs`, `tests/support/upstream.rs`
- Preserve from #95 unless conflict resolution requires it: `src/server.rs`; extend the shared `tests/e2e.rs` target without reverting #95 coverage.

**Interfaces:**
- Consumes: reviewed #95 nonstream commit and the existing Chat contract.
- Produces: one shared event constructor and first-terminal-transition SSE emission.

- [ ] Reconcile #95 and run `chat_nonstream_accounting`; it must prove retry cardinality/payload and response independence before this task edits shared code.
- [ ] Write failing `chat_stream_terminal_accounting`: no pending event and exactly one success/upstream-error/cancelled event across before-header failure, EOF, body error, and drop.
- [ ] Reuse the #95 event constructor. Make the terminal state transition return whether it won; only the winner offers the event through the existing producer API.
- [ ] Run nonstream, stream, and `chat_disconnect_no_replay`; expected PASS.
- [ ] Commit: `feat(chat): account streamed terminal attempts`.

### Task 7: Prove saturation and targeted faults

**Files:**
- Modify: `tests/e2e_accounting.rs`, `tests/support/process.rs`, `tests/support/upstream.rs`, `tests/mutations/` (only reviewed focused patches if cargo-mutants cannot express the fault), `.github/workflows/ci.yml`

- [ ] Implement `chat_accounting_does_not_delay_response` with a test-owned SQLite lock, capacity one, and held tail. Prove SSE progress before release, journal acknowledgement, exact-once reconciliation, and the fallback-unavailable incident response. No production control is added.
- [ ] Run the exact test; expected PASS. A timeout/conflict fixture also PASSes only when retained/unreconciled evidence is observed.
- [ ] Add #98 targeted mutation evidence for early SSE emission, disabling incident admission, and retiring before durable completion. Record caught/survived/equivalent/unviable/timeout honestly at the exact SHA.
- [ ] Commit: `test(m1): qualify nonblocking accounting faults`.

### Task 8: Build the secured guided/headless gate

**Files:**
- Create: `scripts/m1_release_gate.py`, `scripts/test_m1_release_gate.py`
- Create/track: `Cargo.lock`
- Modify: `scripts/sdk_smoke.py`, `tests/requirements-sdk.txt`, `docs/m1-acceptance.md`, `README.md`, `Makefile`, `.github/workflows/ci.yml`, `Dockerfile`

- [ ] Write standard-library tests for loopback-only bind, POST-only state change, exact Host/Origin and capability token, foreign-request rejection, output escaping, fixed command allowlist, one-run exclusion, subprocess deadline/process-group kill, child cleanup, JSON evidence, and separate functional/release verdicts.
- [ ] Reuse `sdk_smoke.py` lifecycle. The page uses `textContent`; no request supplies a command. Every allowlisted subprocess has a deadline and process group; reap it plus fixture/daemon children on every exit path.
- [ ] Replace every Python `assert` used for gate evidence with an explicit `require(condition, message)` that raises `RuntimeError`; run SDK and gate checks under `PYTHONOPTIMIZE=1` so optimization cannot bypass evidence.
- [ ] Track the root `Cargo.lock`. Shared Make setup for `make m1-demo` and `make m1-release-gate` creates `target/m1-sdk-venv`, installs/verifies the exact SDK versions, provisions an absolute temporary accounting root, requires a clean exact candidate, and builds with `cargo build --locked --all-features` into a candidate-specific target directory. Change the Docker build stage to `cargo build --locked --release`; CI fails if a build changes the tracked lockfile. Guided uses `--guided`; release uses `--headless`.
- [ ] The fixed hosted step runs `gh pr view 619 --repo djh00t/steve --json headRefOid,url` and `gh pr checks 619 --repo djh00t/steve --required --json name,state,bucket,link,workflow`, records both JSON results, and requires the PR head to equal the candidate SHA with a nonempty all-passing required-check set. Missing auth/network/checks, unreadable data, pending/failure or mismatch yields unknown/ineligible.
- [ ] `M1_TARGET_PARENT=/existing/deployment/parent M1_DEPLOYMENT_PATH=/intended/accounting/root make m1-target-qualification` accepts an existing target directory/mount and an intended absolute root beneath it. It canonicalizes the parent and lexically normalizes and validates the intended path without touching that root, creates a new empty private probe directory under the parent, proves they share a filesystem, runs qualification only in the probe, records the intended deployment path and distinct probe path, and cleans up the probe. Refuse the live root, a probe equal to the intended root, or any pre-existing/nonempty probe directory. It writes `target/m1-target-qualification.json` with candidate SHA, binary hash/path, intended deployment path, parent/mount and filesystem identity, probe path, tool versions and every named process result. Headless CI writes `target/m1-release-gate.json`, embeds hosted and target artifacts, and, when an adoption manifest exists, copies its maintenance-assertion fields and digest while labeling the external stop unverified. The combined gate surfaces that prerequisite and uncertainty in its single operator step; it adds no supervisor adapter or separate approval. It builds/runs only its recorded binary and requires candidate SHA, binary hash and intended deployment path to match; the temporary probe path is never compared as the deployment identity. Missing, failed, unknown or mismatched evidence stays release-ineligible.
- [ ] Change `docs/m1-acceptance.md` from working-baseline status to final acceptance only when the final gate's exact evidence exists. Keep raw HTTP, official SDK, mutation, target qualification, and hosted evidence distinct.
- [ ] Run the runner unittest, `PYTHONOPTIMIZE=1` combined SDK smoke, `make m1-release-gate`, then `make m1-demo`. Expected: functional PASS; release eligibility reflects actual hosted/target evidence rather than treating expected negative scenarios as failures.
- [ ] Commit: `feat(m1): add secured combined release gate`.

### Task 9: Freeze the candidate and ask once

**Files:**
- Modify only evidence/docs errors exposed by Tasks 1-8.

- [ ] On a frozen candidate run all named accounting tests, existing Chat/Responses/Messages/cancellation/models/provider-health tests, SDK smoke, runner unittest, `git diff --check`, and `make check`. Do not run local `make quality-gates`.
- [ ] Push PR #619. Run the fixed `gh pr view`/`gh pr checks` evidence commands; require exact head equality and a nonempty all-passing required-check set. Verify mutation outcomes, review threads, and uploaded evidence at that head. Do not merge or approve.
- [ ] On the deployment host, run `M1_TARGET_PARENT=/existing/deployment/parent M1_DEPLOYMENT_PATH=/intended/accounting/root make m1-target-qualification` and attach `target/m1-target-qualification.json`. Require its candidate SHA, binary hash and intended deployment path to match the gate, and require its isolated probe to be distinct and on the same filesystem. Never run the probe against a live accounting root. This qualifies process behavior only; record OS crash and power loss as unknown unless separately proven.
- [ ] Launch the guided page. The operator reviews one combined summary and accepts or rejects M1 once.
