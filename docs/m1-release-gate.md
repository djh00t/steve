# M1 release gate

## Outcome

M1 has one operator acceptance point: a loopback browser walkthrough over one exact candidate SHA. It starts the real daemon and deterministic upstream, runs the official SDK journeys and focused real-process scenarios, and exports one evidence summary. Engineering slices are reviewed without asking the operator to accept each deliverable.

The browser has two separate verdicts:

- **Functional run:** the local scenarios behaved as required.
- **Release eligible:** the functional run passed, required hosted checks passed at the same SHA, and the intended deployment OS/filesystem/path has a matching target-qualification artifact produced in an isolated probe directory on that filesystem. Missing, unreadable, pending, failed, or SHA/binary/deployment-path-mismatched evidence makes this verdict `unknown` and the candidate ineligible.

An expected conflict, timeout, or unknown outcome is a passing negative scenario when Steve retains it, reports it, and blocks admission as specified. It is not a successful accounting outcome.

## Required evidence

| Area | Observable result | Issues |
| --- | --- | --- |
| Protocols | Official OpenAI Chat and Responses and official Anthropic Messages streaming helpers complete through the real local daemon. | #37, #98 |
| Streaming | First bytes precede the held tail; disconnect cancels upstream work and never replays after output starts. | #37, #98 |
| Chat accounting | The separately owned #95 nonstream slice emits one `chat.attempt.terminal.v1` per terminal retry attempt. #96 emits no SSE event while pending and exactly one for EOF, upstream error, or disconnect. | #45, #94-#96 |
| Framing and replay | Complete frames before a torn tail survive; temporary DB failure follows #86's configured per-operation bound; inserted and identical duplicates complete; conflicting or failed records remain retained. | #85-#87 |
| Incident policy | An admitted response may finish after accounting failure; later inference gets `503 accounting_incident`; readiness is false while liveness/status remain available. Verified reconciliation returns to `clear`. Confirmed loss or irreducible uncertainty requires an auditable operator disposition and restores as redacted `acknowledged` after restart. | #84, #483, #485 |
| Conflict recovery | Exact protected evidence is inspectable only through the local read-only audit command. Resolution is offline, selects journal or database as authoritative, verifies both sides, and cannot use loss/uncertainty acknowledgement as a bypass. | #87, #483, #485 |
| Ownership | Fresh provisioning creates a stable installation identity and initial evidence. Revision-checked coordination and generation coverage permit replacement ready before healthy old exit, reject missing/expired evidence, and never inspect a locked live journal. Legacy flat-file adoption is explicit and offline, with an operator-supplied maintenance assertion for the external writer stop prerequisite. | #84, #484, #485 |
| Shutdown | Management drain and process signal enter the same accounting-drain sequence under one absolute deadline. Management drain preserves its current non-terminating semantics; signal exits after the same barriers. Timeout evidence survives restart. | #84, #485 |
| Saturation | A test-owned SQLite write lock fills capacity one; SSE progresses before release, then its stable event reconciles once. No production fault-control endpoint or flag exists. | #45, #97 |
| Catalogue and health | Models, provider health, readiness, liveness, and status retain their contracts. | #37, #98 |

## Ownership, adoption, and durability boundary

The #483/#484 selections and #485 completion contract are working design baselines. One explicitly provisioned absolute private root contains a stable installation identity, a coordination lock, revisioned incident evidence, per-generation state and locked journals. A coordination update rereads the revision and records coverage tuples `(generation_id, generation_state_revision, journal_evidence_digest)`. A snapshot is usable only within the working-baseline freshness window; a stale revision, expired snapshot, missing/corrupt artifact, unsupported path, or sync/lock/publication error fails closed. A healthy busy predecessor journal is left untouched.

The existing relative flat journal is never adopted automatically. Offline adoption requires the operator to stop every pre-protocol writer externally and disable its restart. `steve accounting adopt-legacy --source FILE --root PATH --maintenance-assertion FILE` retains an unauthenticated operator assertion naming the source path, source host, assertion time, supervisor context, and claimed stop/disable action; its digest supports audit integrity only. When `<root>/adoption.json` or its checksum exists (or `M1_ADOPTION_MANIFEST` explicitly names an expected manifest), the gate verifies the Rust manifest checksum and the `maintenance_assertion` fields (`assertion_kind`, `path`, `digest`, `workload_identity`, `host`, `source`, `stopped`, `restart_disabled`, `observed_at`, `command_or_exported_status`) against the retained assertion file, then copies its fields and digest into `target/m1-release-gate.json` and clearly surfaces that the external stop remains an operator prerequisite Steve cannot verify. A missing or incomplete assertion makes adoption and release eligibility fail closed, but assertion presence is not verified stop evidence. The new coordination guard covers cooperating participants only; neither guard success nor a source hash/length recheck proves that an uncooperative old binary cannot append later. After the assertion exists, adoption hashes and syncs a backup, provisions a distinct root, and imports under a locked generation using #85 framing. An empty source may complete vacuously. A nonempty source remains staged and startup-blocking, with source and backup retained, until the Task 3 replay path produces #485 acknowledgements; only then may it sync the adoption marker and move the unchanged source to a retained name. Missing assertion or any incomplete migration evidence fails closed and resumes from the backup. The combined gate presents this external prerequisite and uncertainty in the single operator acceptance step; it does not add a separate adoption approval.

Completion has narrow meanings:

- journal acknowledgement: frame write, flush, and file sync completed;
- database acknowledgement: inserted or byte-identical duplicate under the backend's qualified durability settings;
- drain acknowledgement: worker and journal barriers cover all earlier accepted messages; and
- retirement: every covered frame is acknowledged, completion/incident evidence and containing directory are synced, and the backend guarantee covers the declared failure domain.

M1 executable evidence covers **process crash only**. OS crash and power-loss durability remain **unknown** and must never be inferred from CI. The gate retains the completed generation rather than retire the only journal copy while either is unknown. Future retirement additionally requires the full backend durability contract: SQLite WAL accounting connections verified at `synchronous=FULL`, or PostgreSQL verified with `fsync=on`, `synchronous_commit=on`, and `full_page_writes=on`, plus separately qualified storage durability for the declared OS/power failure domain.

Both drain triggers use one absolute deadline calculated once from `server.drain_timeout_seconds`; no stage resets it. Before the final admitted body or accounting write can fail, Steve syncs generation state that makes restart conservative. It then stops inference admission, lets admitted bodies finish until the deadline, cancels what remains, closes accounting producers, runs worker and journal barriers with the remaining time, and persists completion or unresolved evidence. Ownership is released only after every writer has stopped and joined. On successful management drain the process remains running and not-ready after releasing the completed generation. On management-drain timeout it remains running, retains the generation lock and unresolved evidence while any writer is alive, and a successor cannot acquire that generation. On signal timeout it retains ownership until process termination closes the file and releases the OS lock. Restart then observes unresolved evidence.

## Guided walkthrough

`scripts/m1_release_gate.py --guided` binds an ephemeral loopback address and opens one page with one **Run M1 gate** action. The runner accepts only fixed allowlisted steps. State changes are POST-only and require the exact loopback `Host`, exact `Origin`, and a random capability token. The page inserts command output with `textContent`, never HTML. Every subprocess has a deadline, runs in a killable process group, and is reaped with its daemon/fixture children on success, failure, disconnect, or interrupt. Foreign Host/Origin/token requests fail.

The page shows daemon readiness, SDK journeys, Chat accounting, cancellation/no-replay, recovery, incident/restart, replacement, shutdown success/timeout, saturation, models/health, targeted mutation outcomes, SHA/dirty state, hosted evidence, target qualification, and unsupported limits. Headless mode runs the same allowlist and writes JSON for CI. It adds no curl, Swagger, frontend dependency, credential, bypass, or production test control. The gate builds the binary itself from a clean exact SHA using the tracked lockfile, records the binary hash and Rust/Cargo/Python/SDK versions, and runs only that binary. Python checks use explicit exceptions rather than `assert` and pass with `PYTHONOPTIMIZE=1`.

Exact issue ownership, files, and focused selector commands are recorded in [the M1 implementation handoff](m1-acceptance.md#integrated-implementation-and-qualification-handoff). None is passing evidence until it exists and passes on the exact candidate SHA.

## Qualification and final operator step

The fixed hosted-evidence step records the PR head and required checks with:

```sh
gh pr view 619 --repo djh00t/steve --json headRefOid,url > target/m1-pr-head.json
gh pr checks 619 --repo djh00t/steve --json name,state,bucket,link,workflow > target/m1-hosted-checks.json
gh api repos/djh00t/steve/rules/branches/main > target/m1-forge-required-checks.json
gh api repos/djh00t/steve/branches/main/protection/required_status_checks > target/m1-legacy-required-checks.json
```

The gate re-reads the PR head after both check queries, retains both head observations, and rejects any change. It requires `headRefOid == git rev-parse HEAD` in both observations and evaluates the reviewed M1 check names plus every check enforced by the repository. Every check in that union must exist and pass. A repository with no enforced status-check rule is recorded separately as unenforced; it does not erase passing reviewed workflow evidence. Missing `gh` authentication/network access, a missing reviewed or enforced check, unreadable JSON, a pending/failed/cancelled required check, or a head mismatch records hosted evidence as unknown or failed and makes release eligibility false.

Run `M1_TARGET_PARENT=/existing/deployment/parent M1_DEPLOYMENT_PATH=/intended/accounting/root make m1-target-qualification` on the deployment host. `M1_TARGET_PARENT` is the existing directory or mount where the deployment root will reside; `M1_DEPLOYMENT_PATH` is the intended absolute root beneath it. The command canonicalizes the existing parent and lexically normalizes and validates the intended path without reading or writing that root, creates a new empty private probe directory under the target parent, verifies that the probe and parent are on the same filesystem, runs the destructive process checks only in that probe, and removes it after recording evidence. It must refuse a probe path equal to the intended root or any pre-existing/nonempty probe directory; a live accounting root is never a qualification target.

The command writes `target/m1-target-qualification.json` containing candidate SHA, candidate binary SHA-256/path, intended deployment path, target parent/mount identity, distinct probe path, OS/kernel, filesystem and storage path, command/tool versions, timestamp, and results for exclusive locking, publication/atomic replacement, append visibility, file and directory sync calls, revision/coverage behavior, and process-death lock release. The release gate requires the artifact's candidate SHA and binary hash to match the binary it runs, its intended deployment path to match the gate configuration, and its probe to be distinct and on the same recorded filesystem. The temporary probe path itself is evidence, not the deployment-path identity used for matching. Missing, unreadable, failed, or mismatched target evidence is `unknown` and release-ineligible. This is process-level evidence. Linux/ext4 evidence does not qualify macOS/APFS, Windows/NTFS, network storage, or another path; OS-crash and power-loss behavior stay unknown unless separately proven.

The gate additionally requires `target/m1-mutation-result.json` with clean exact-head caught outcomes for early SSE emission, disabled incident admission, and premature retirement, plus the dedicated `m1-mutation` hosted check. CI downloads this artifact from the prerequisite mutation job; a generic mutation pass cannot substitute.

Manual probes default to `deployment-target` evidence scope. The Windows CI job probes actual local NTFS under the runner temporary directory with native Win32 handles and records `m1-windows-ntfs-ci-process-semantics.json` using `ci-runner-process-semantics` scope, bound to the exact candidate SHA and binary hash. The deployment-target loader rejects that scope even if the artifact is renamed or copied. A failed native file or directory/metadata sync fails qualification; unsupported directory sync is never a passing result. Windows CI process evidence does not qualify a real deployment path, OS crash or power loss, or final operator acceptance; Windows/NTFS deployment qualification remains unknown until the manual probe passes on the intended host/path.

The gate embeds these JSON artifacts and their verdicts in `target/m1-release-gate.json`. After PR #619 is ready for review, hosted checks and mutation evidence exist at its exact head, and target functional qualification matches, the operator reviews the single browser summary and exported JSON. The release document's earlier proposal/approval wording is updated only at this final gate. The operator accepts or rejects M1 once; the gate never merges, deploys, acknowledges an accounting incident, or records release acceptance for them.
