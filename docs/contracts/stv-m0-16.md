# STV-M0-16: accounting incident and admission contract proposal

**Status: proposal for David/Cos review; not accepted.** This preserves #84's accepted choice: preserve forwarding, fail visibly, stop later inference. It proposes externally visible semantics only. #484 owns storage ownership and replacement mechanisms; #485 owns drain/replay acknowledgements. No mechanism or persistence guarantee is specified here.

Source review: verified against current main `675533dbe0e2d00af9ec71ca59eb34d9151ba63f`. Relevant seams are `src/deferred.rs::{QueueStats, spill_accounting_event}`, `src/server.rs::{ready, status, admit_inference}`, and `src/lifecycle.rs`. Current `accounting_spilled` means accepted by the journal channel, not written or durable; `accounting_lost` is a counter. Current readiness only reflects lifecycle phase, and inference admission has no accounting-failure gate. This is source-review context only; it makes no runtime guarantee.

## Proposed state and transition table

| State | Entry / transition | Inference admission | Readiness | Meaning |
|---|---|---|---|---|
| `clear` | Startup after the ownership/recovery contract establishes no unresolved incident for this logical accounting owner; remains clear while the primary or monitored fallback path accepts responsibility. A healthy overlapping predecessor is not itself an incident. | Admit normally. | `200 ready` when lifecycle is ready. | No known outstanding accounting incident. |
| `blocked` | First observed failure to enqueue to the primary accounting queue and the journal channel, including an asynchronous primary-queue acceptance followed by database-write and journal-enqueue failure, or a journal write/flush failure. Remains until verified reconciliation returns it to clear, or an explicit recorded disposition moves it to acknowledged. | Requests whose admission linearizes after the incident latch get `503 accounting_incident`; requests already admitted may finish forwarding. | `503 not_ready`, while liveness remains `200`. | Later inference is stopped, even when another payload is retained. |
| `acknowledged` | An authorized operator explicitly accepts confirmed or irreducible possible loss, after all retained work is reconciled and the disposition is recorded under #484/#485. Verified durable acknowledged state is restored as acknowledged after restart. | Admit normally, subject to lifecycle and capacity. | `200 ready` when lifecycle is ready. | The incident remains visible with its terminal loss and any provisional uncertainty classifications; acceptance is not recovery. |
| `unreconciled` | A prior unresolved incident is known, or crash/ownership evidence cannot establish a trustworthy incident state; remains until verified reconciliation or an explicit recorded disposition. Missing/unverifiable acknowledgement evidence also enters this state. Ordinary healthy overlap alone does not enter this state. | Reject with the same `503 accounting_incident`. | `503 not_ready`; liveness remains `200`. | Restart alone cannot erase an incident or establish recovery. |

The local cutoff is when a serving process observes the failure and latches `blocked`, serialized with that process's admission. This does not promise an instantaneous cluster-wide cutoff before another process observes the incident. #484 must define the logical owner identity, overlap propagation and restart handoff before implementation is ready. A request admitted before that point may finish normally, including its existing upstream forwarding. A failure discovered asynchronously cannot revoke already admitted work; once observed, subsequent inference admission is rejected while the incident remains blocked or unreconciled. Management status and recovery inspection remain available subject to existing management admission limits.

## Proposed status and error contract

Keep `GET /api/v1/system/status` HTTP `200` while the process can answer. Add this required object; do not infer incident state from `queues.accounting_spilled` alone:

```text
"accounting_incident": {
  "state": "clear | blocked | unreconciled | acknowledged",
  "incident_id": "string | null",
  "first_observed_at": "RFC3339 string | null",
  "cause": "primary_and_journal_unavailable | primary_persistence_failed_and_journal_unavailable | journal_write_failed | prior_incident_unreconciled | null",
  "disposition": {"kind":"accepted_loss | accepted_uncertainty | accepted_loss_and_uncertainty","actor":"string | null","at":"RFC3339 string","evidence_ref":"string | null"} | null,
  "payloads": {
    "pending_replay": {
      "volatile": "u64 | null",
      "durable": "u64 | null"
    },
    "provisional": {
      "unknown": "u64 | null"
    },
    "outcome_totals": {
      "reconciled": "u64 | null",
      "unrecoverable_lost": "u64 | null"
    }
  }
}
```

These counts describe payloads attributed to the current incident, not ordinary healthy queue occupancy or lifetime totals. `pending_replay` counts payloads still awaiting replay acknowledgement: `volatile` means only an in-memory queue/channel currently retains them, while `durable` means the persistence owner has confirmed durable retention. `provisional.unknown` counts payloads whose write/crash outcome is not yet established; it is not a terminal outcome total. `outcome_totals` counts terminal incident outcomes and is not part of pending replay: `reconciled` means replay/reconciliation completed and the payload is no longer replayable, while `unrecoverable_lost` means the payload was discarded and cannot be reconstructed. A payload moves from pending replay to either `provisional.unknown` or exactly one terminal outcome; once evidence resolves `provisional.unknown`, it moves exactly once to `outcome_totals.reconciled` or `outcome_totals.unrecoverable_lost` and is removed from `provisional.unknown`. An authorized disposition may leave irreducible uncertainty in `provisional.unknown`; it must not relabel that payload as reconciled or lost. Use `primary_persistence_failed_and_journal_unavailable` when the primary queue accepted an event, its later database write failed, and the journal enqueue also failed. Prior incidents are retained in durable audit history outside this single current-status object; the disposition evidence reference identifies the corresponding audit evidence. The object is one internally consistent incident snapshot. This block is a type sketch, not a literal wire fixture. `disposition` is null except in `acknowledged`, where all fields are required in the wire object and non-null in the protected durable audit record: the authenticated/operator identity, UTC disposition timestamp, and an auditable evidence reference must be recorded before admission reopens. On public status and health responses, `actor` and `evidence_ref` are always null (redacted), including after management authentication is added; inspect their full values only through a separately authorized audit-read mechanism defined by #484/#485. Do not expose identities, storage paths, payload content, or audit references through these public routes. These two redacted nulls mean withheld, not missing audit evidence. #484/#485 own the durable recording and acknowledgement mechanism; no unauthenticated acknowledgement API is introduced here. All fields are required. Null metadata in clear means no current incident/disposition. In other states, `null` means the exact value is unknown or cannot yet be established; it never means zero. Counts are exact known counts, and must not be derived from event IDs or channel-send success. When `state == clear`, `incident_id`, timestamps and cause are null and every pending, provisional and outcome count is zero; disposition is null. In `acknowledged`, retain the incident identity, cause, terminal outcome totals, any accepted provisional uncertainty and non-null disposition, while pending replay counts are zero because acknowledgement requires the replay barrier. Unknown or lost data must not be relabeled as reconciled merely to reopen admission. For non-clear states, use null where evidence is unavailable.

Concrete blocked-state fixture (illustrative IDs/timestamp; no event payload content):

```json
{"accounting_incident":{"state":"blocked","incident_id":"01995200-0000-7000-8000-000000000001","first_observed_at":"2026-09-27T08:00:00Z","cause":"primary_and_journal_unavailable","disposition":null,"payloads":{"pending_replay":{"volatile":0,"durable":0},"provisional":{"unknown":0},"outcome_totals":{"reconciled":0,"unrecoverable_lost":1}}}}
```

Concrete unreconciled-state fixture (identity cannot be established after restart; the null incident ID is intentional):

```json
{"accounting_incident":{"state":"unreconciled","incident_id":null,"first_observed_at":null,"cause":"prior_incident_unreconciled","disposition":null,"payloads":{"pending_replay":{"volatile":null,"durable":null},"provisional":{"unknown":null},"outcome_totals":{"reconciled":null,"unrecoverable_lost":null}}}}
```

An incident ID is an opaque stable identifier for the incident, not a recoverable event payload. Pending replay counts and terminal outcome totals are separate and must not be added to a lifetime queue counter. If the number of ambiguous payloads is unknowable, `unknown` is null, not zero.

Literal inference rejection fixtures (all inference routes except existing GET health exemptions, when lifecycle is not draining; `state` is derived from the latched incident and `incident_id` is nullable for both `blocked` and `unreconciled`). Both are HTTP `503 Service Unavailable` with `Content-Type: application/json`, `Cache-Control: no-store`, and `Retry-After: 1`:

```json
{"error":{"type":"unavailable","code":"accounting_incident","message":"inference admission stopped by an unresolved accounting incident","incident_id":null,"state":"blocked"}}
```

```json
{"error":{"type":"unavailable","code":"accounting_incident","message":"inference admission stopped by an unresolved accounting incident","incident_id":null,"state":"unreconciled"}}
```

Preserve the established ordering: GET health exemptions, existing lifecycle-draining behavior, incident gate, then capacity admission. Do not replace protocol-specific draining responses with this error. The response says admission was stopped; it does not claim the triggering request was rolled back or that its payload is recoverable. `GET /health/ready` returns `503` only for `blocked` or `unreconciled` and adds the same required `accounting_incident` object shown above; `GET /health/live` remains `200` while the process responds. `acknowledged` and `clear` return ready only when the existing lifecycle is ready. This preserves readiness/liveness distinction.

Literal `GET /health/ready` response fixture: HTTP 503. The lifecycle may still be ready while the separate accounting gate blocks new inference; the already admitted request remains active.

```json
{"status":"not_ready","phase":"ready","inflight":1,"admission":{"inference":{"limit":32,"active":1,"rejected_total":0},"management":{"limit":4,"active":0,"rejected_total":0}},"accounting_incident":{"state":"blocked","incident_id":"01995200-0000-7000-8000-000000000001","first_observed_at":"2026-09-27T08:00:00Z","cause":"primary_and_journal_unavailable","disposition":null,"payloads":{"pending_replay":{"volatile":0,"durable":0},"provisional":{"unknown":0},"outcome_totals":{"reconciled":0,"unrecoverable_lost":1}}}}
```

## Recovery and incident lifetime proposal

**Proposal pending David/Cos acceptance: recommend sticky incident semantics across process restart**, represented as `unreconciled` when a known unresolved incident or an ambiguous crash state is inherited by the same logical owner, until verified recovery evidence closes it. Healthy replacement remains eligible for ready-before-old-exit; the mere presence of an active predecessor is not failure evidence. This avoids silently reopening inference after a restart that may have followed loss. Tradeoff: startup/readiness can remain blocked until #484/#485 provide ownership and recovery evidence; #483 does not prescribe how that evidence is stored or handed across overlapping processes.

Acknowledgement requires an internally consistent closure snapshot: every volatile item must have been reconciled or truthfully classified as irrecoverably lost/unknown under #484/#485 evidence, and no known retained item may be discarded or relabeled merely to open admission. Pending replay counts must be zero before acknowledgement; terminal outcome totals and any accepted provisional uncertainty remain attached to the incident as its outcome record. A verified durable acknowledged record restores as acknowledged, preserving outcome totals, provisional uncertainty and disposition; missing or unverifiable acknowledgement restores as unreconciled, never clear.

Only confirmed durable retained payloads can be called replayable; resumption requires replay acknowledgement for those payloads plus an explicit disposition for known or irreducibly uncertain lost payloads. A known lost payload stays in the `unrecoverable_lost` outcome total; accepting that loss requires an explicit operator decision/evidence before reopening admission in acknowledged state, and is not automatic. For ambiguous crash/write outcomes, remain `unreconciled` with a provisional `unknown` count until evidence resolves each payload or an authorized operator explicitly accepts the irreducible uncertainty. Evidence moves each resolved provisional payload exactly once to either `reconciled` or `unrecoverable_lost`; acceptance of irreducible uncertainty keeps it provisional and records `accepted_uncertainty`. Confirmed unrecoverable loss alone uses `accepted_loss`. When both confirmed loss and irreducible uncertainty are accepted for the same incident, use `accepted_loss_and_uncertainty` and preserve both classifications. `accepted_uncertainty` alone is valid only when no confirmed loss is accepted. Recovery evidence alone may return a fully reconciled incident to `clear`; operator acceptance never claims recovery. A new failure from `acknowledged` opens/latches a new blocking incident and preserves the prior disposition as auditable history under #484/#485. An event ID cannot reconstruct a discarded payload, and successful enqueue to either bounded channel is not proof of durability.

## Examples and consumer inputs

- **Positive:** both enqueue paths fail for event E. The request already admitted may finish; later inference gets the specified 503. Status reports a non-clear incident and `outcome_totals.unrecoverable_lost >= 1` only if E is confirmed discarded; otherwise E is in `provisional.unknown`.
- **Positive:** the primary queue accepts E, its asynchronous database write later fails, and the journal enqueue also fails. Latch the incident when that second failure is observed; the already admitted request may finish, later inference gets the specified 503, and E is in `provisional.unknown` unless evidence confirms loss. A journal enqueue that succeeds would leave E in `pending_replay.durable` only after durable retention is confirmed; channel acceptance alone never proves that.
- **Positive:** database write fails and the monitored journal path accepts E. Fallback use alone leaves incident state clear; E remains ordinary healthy fallback backlog outside `accounting_incident`, so all incident counts stay zero. If a later journal failure opens an incident, only then are affected payloads classified under the incident contract.
- **Positive:** replacement of the same logical owner discovers a known unresolved prior incident. It reports `unreconciled`, readiness 503, and rejects inference until recovery evidence closes the incident.
- **Negative:** status reports `accounting_spilled > 0` as proof that all affected events are durable/replayable.
- **Negative:** a known unresolved incident is reset merely by restart, or an event ID is presented as reconstruction of a lost payload.

Consumers #94 and accounting implementation briefs should consume these exact state names, nullable-count rules, cutoff, 503 code, and recovery distinction only after David/Cos accepts this artifact. #484 must supply the mechanism and ownership that can truthfully establish cross-restart state; #485 must define replay acknowledgement and evidence. They must not weaken the accepted forwarding choice or assume ready-before-old-exit is overridden.

## Executable consumer qualification gate (not implemented)

This proposal does not supply a runnable runtime qualification today and cannot unblock consumers by itself. Once #483/#484/#485 are accepted, the consumer handoff must add `tests/e2e_accounting_incident.rs` with the real-process scenario `accounting_incident_preserves_forwarding_and_restart_evidence`. Its exact qualification command is:

```sh
cargo test --all-features --test e2e_accounting_incident accounting_incident_preserves_forwarding_and_restart_evidence -- --exact --nocapture
```

The target does not exist yet; this command is a reserved consumer contract, not passing evidence. Dispatch requires an implementation brief owning that target and the accepted fault-injection/restart seams. Completion requires exactly one selected, passing scenario (zero selected is failure). Given an already admitted held upstream response, force both accounting paths to fail; prove the response still completes, later inference gets the exact 503, readiness is false, liveness/status remain available, and public disposition identity/reference fields are redacted. Restart the same owner and prove the accepted incident policy survives, then exercise the accepted recorded disposition and mixed-loss/uncertainty state. Fail the test when the incident gate or restart evidence check is deliberately bypassed. The accepted ownership/completion contracts must make those seams deterministic before this scenario is implemented; no placeholder test or simulated passing evidence is sufficient.

**Unresolved acceptance question (proposal pending David/Cos acceptance):** Keep incidents blocked across restart, but permit an authorized operator to explicitly accept confirmed or irreducible possible loss and resume with a visible `acknowledged` incident? Recommendation: yes. The alternatives are restart clearing the incident, or requiring proven recovery even when uncertainty is irreducible (potentially permanent blocking). This extends the settled forwarding policy; it is not accepted until David/Cos decides.

#484 acceptance dependency: demonstrate affirmative no-incident/reconciled evidence for healthy overlapping replacement while the predecessor is alive; old-process exit cannot be the only proof. The proposed acknowledgement must remain auditable through restart. #485 defines completion acknowledgements; implementation is blocked until both mechanisms are accepted and qualified.
