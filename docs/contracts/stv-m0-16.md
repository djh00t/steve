# STV-M0-16: accounting incident and admission contract proposal

**Status: proposal for David/Cos review; not accepted.** This preserves #84's accepted choice: preserve forwarding, fail visibly, stop later inference. It proposes externally visible semantics only. #484 owns storage ownership and replacement mechanisms; #485 owns drain/replay acknowledgements. No mechanism or persistence guarantee is specified here.

Source review: requested main reference `6d635f5`; current source checkout `ddfcea6`. Relevant seams are `src/deferred.rs::{QueueStats, spill_accounting_event}`, `src/server.rs::{ready, status, admit_inference}`, and `src/lifecycle.rs`. Current `accounting_spilled` means accepted by the journal channel, not written or durable; `accounting_lost` is a counter. Current readiness only reflects lifecycle phase, and inference admission has no accounting-failure gate.

## Proposed state and transition table

| State | Entry / transition | Inference admission | Readiness | Meaning |
|---|---|---|---|---|
| `clear` | Startup after the ownership/recovery contract establishes no unresolved incident for this logical accounting owner; remains clear while the primary or monitored fallback path accepts responsibility. A healthy overlapping predecessor is not itself an incident. | Admit normally. | `200 ready` when lifecycle is ready. | No known outstanding accounting incident. |
| `blocked` | First observed failure to enqueue to the primary accounting queue and the journal channel, or a journal write/flush failure. Remains until verified reconciliation returns it to clear, or an explicit recorded disposition moves it to acknowledged. | Requests whose admission linearizes after the incident latch get `503 accounting_incident`; requests already admitted may finish forwarding. | `503 not_ready`, while liveness remains `200`. | Later inference is stopped, even when another payload is retained. |
| `acknowledged` | An authorized operator explicitly accepts confirmed or irreducible possible loss, after all retained work is reconciled and the disposition is recorded under #484/#485. Verified durable acknowledged state is restored as acknowledged after restart. | Admit normally, subject to lifecycle and capacity. | `200 ready` when lifecycle is ready. | The incident remains visible with its original lost/unknown classifications; acceptance is not recovery. |
| `unreconciled` | A prior unresolved incident is known, or crash/ownership evidence cannot establish a trustworthy incident state; remains until verified reconciliation or an explicit recorded disposition. Missing/unverifiable acknowledgement evidence also enters this state. Ordinary healthy overlap alone does not enter this state. | Reject with the same `503 accounting_incident`. | `503 not_ready`; liveness remains `200`. | Restart alone cannot erase an incident or establish recovery. |

The local cutoff is when a serving process observes the failure and latches `blocked`, serialized with that process's admission. This does not promise an instantaneous cluster-wide cutoff before another process observes the incident. #484 must define the logical owner identity, overlap propagation and restart handoff before implementation is ready. A request admitted before that point may finish normally, including its existing upstream forwarding. A failure discovered asynchronously cannot revoke already admitted work; once observed, subsequent inference admission is rejected while the incident remains blocked or unreconciled. Management status and recovery inspection remain available subject to existing management admission limits.

## Proposed status and error contract

Keep `GET /api/v1/system/status` HTTP `200` while the process can answer. Add this required object; do not infer incident state from `queues.accounting_spilled` alone:

```text
"accounting_incident": {
  "state": "clear | blocked | unreconciled | acknowledged",
  "incident_id": "string | null",
  "first_observed_at": "RFC3339 string | null",
  "cause": "primary_and_journal_unavailable | journal_write_failed | prior_incident_unreconciled | null",
  "disposition": {"kind":"accepted_loss | accepted_uncertainty | accepted_loss_and_uncertainty","actor":"string | null","at":"RFC3339 string","evidence_ref":"string | null"} | null,
  "payloads": {
    "volatile_pending": "u64 | null",
    "durable_replayable": "u64 | null",
    "unrecoverable_lost": "u64 | null",
    "unknown": "u64 | null"
  }
}
```

These counts describe payloads attributed to the current incident, not ordinary healthy queue occupancy or lifetime totals. Prior incidents are retained in durable audit history outside this single current-status object; the disposition evidence reference identifies the corresponding audit evidence. The object is one internally consistent incident snapshot. This block is a type sketch, not a literal wire fixture. `disposition` is null except in `acknowledged`, where all four fields are required in the wire object and non-null in the protected durable audit record: the authenticated/operator identity, UTC disposition timestamp, and an auditable evidence reference must be recorded before admission reopens. On public status and health responses, `actor` and `evidence_ref` are always null (redacted), including after management authentication is added; inspect their full values only through a separately authorized audit-read mechanism defined by #484/#485. Do not expose identities, storage paths, payload content, or audit references through these public routes. These two redacted nulls mean withheld, not missing audit evidence. #484/#485 own the durable recording and acknowledgement mechanism; no unauthenticated acknowledgement API is introduced here. All fields are required. Null metadata in clear means no current incident/disposition. In other states, `null` means the exact value is unknown or cannot yet be established; it never means zero. Counts are exact known counts, and must not be derived from event IDs or channel-send success. `volatile_pending` means payload is still only in an in-memory queue/channel and is not promised to survive a crash. `durable_replayable` is allowed only after the persistence owner confirms the payload was durably retained and is awaiting replay/acknowledgement. `unrecoverable_lost` means the payload is known to have been discarded and cannot be reconstructed. `unknown` covers ambiguous writes or crash windows. Keep each category separate; do not collapse lost and retained counts into one recovery label. When `state == clear`, `incident_id`, timestamps and cause are null and all four counts are zero; disposition is null. In `acknowledged`, retain the incident identity, cause, counts and non-null disposition. Unknown or lost data must not be relabeled as recovered merely to reopen admission. For non-clear states, use null where evidence is unavailable.

Concrete blocked-state fixture (illustrative IDs/timestamp; no event payload content):

```json
{"accounting_incident":{"state":"blocked","incident_id":"01995200-0000-7000-8000-000000000001","first_observed_at":"2026-09-27T08:00:00Z","cause":"primary_and_journal_unavailable","disposition":null,"payloads":{"volatile_pending":0,"durable_replayable":0,"unrecoverable_lost":1,"unknown":0}}}
```

An incident ID is an opaque stable identifier for the incident, not a recoverable event payload. Known categories are disjoint current classifications of incident-attributed payloads; do not add a lifetime queue counter to these counts. If the number of ambiguous payloads is unknowable, `unknown` is null, not zero.

Literal inference rejection fixture (all inference routes except existing GET health exemptions, when lifecycle is not draining; `incident_id` remains nullable when identity cannot be established): HTTP `503 Service Unavailable`, `Content-Type: application/json`, `Cache-Control: no-store`, `Retry-After: 1`:

```json
{"error":{"type":"unavailable","code":"accounting_incident","message":"inference admission stopped by an unresolved accounting incident","incident_id":"01995200-0000-7000-8000-000000000001","state":"blocked"}}
```

Preserve the established ordering: GET health exemptions, existing lifecycle-draining behavior, incident gate, then capacity admission. Do not replace protocol-specific draining responses with this error. The response says admission was stopped; it does not claim the triggering request was rolled back or that its payload is recoverable. `GET /health/ready` returns `503` only for `blocked` or `unreconciled` and adds the same required `accounting_incident` object shown above; `GET /health/live` remains `200` while the process responds. `acknowledged` and `clear` return ready only when the existing lifecycle is ready. This preserves readiness/liveness distinction.

Literal `GET /health/ready` response fixture: HTTP 503. The lifecycle may still be ready while the separate accounting gate blocks new inference; the already admitted request remains active.

```json
{"status":"not_ready","phase":"ready","inflight":1,"admission":{"inference":{"limit":32,"active":1,"rejected_total":0},"management":{"limit":4,"active":0,"rejected_total":0}},"accounting_incident":{"state":"blocked","incident_id":"01995200-0000-7000-8000-000000000001","first_observed_at":"2026-09-27T08:00:00Z","cause":"primary_and_journal_unavailable","disposition":null,"payloads":{"volatile_pending":0,"durable_replayable":0,"unrecoverable_lost":1,"unknown":0}}}
```

## Recovery and incident lifetime recommendation

**Recommend sticky incident semantics across process restart**, represented as `unreconciled` when a known unresolved incident or an ambiguous crash state is inherited by the same logical owner, until verified recovery evidence closes it. Healthy replacement remains eligible for ready-before-old-exit; the mere presence of an active predecessor is not failure evidence. This avoids silently reopening inference after a restart that may have followed loss. Tradeoff: startup/readiness can remain blocked until #484/#485 provide ownership and recovery evidence; #483 does not prescribe how that evidence is stored or handed across overlapping processes.

Acknowledgement requires an internally consistent closure snapshot: every volatile item must have been reconciled or truthfully classified as irrecoverably lost/unknown under #484/#485 evidence, and no known retained item may be discarded or relabeled merely to open admission. Pending retained work prevents acknowledgement; #485 supplies the completion barrier and concurrent-failure rule. A verified durable acknowledged record restores as acknowledged, preserving counts/disposition; missing or unverifiable acknowledgement restores as unreconciled, never clear.

Only confirmed durable retained payloads can be called replayable; resumption requires replay acknowledgement for those payloads plus an explicit disposition for known or irreducibly uncertain lost payloads. A known lost payload stays `unrecoverable_lost`; accepting that loss requires an explicit operator decision/evidence before reopening admission in acknowledged state, and is not automatic. For ambiguous crash/write outcomes, remain `unreconciled`/`unknown` until evidence resolves them or an authorized operator explicitly accepts the irreducible uncertainty. Such acceptance moves to `acknowledged`, preserves unknown counts as unknown, and records `accepted_uncertainty`; confirmed unrecoverable loss alone uses `accepted_loss`. When both confirmed loss and irreducible uncertainty are accepted for the same incident, use `accepted_loss_and_uncertainty` and preserve both classifications. `accepted_uncertainty` alone is valid only when no confirmed loss is accepted. Recovery evidence alone may return a fully reconciled incident to `clear`; operator acceptance never claims recovery. A new failure from `acknowledged` opens/latches a new blocking incident and preserves the prior disposition as auditable history under #484/#485. An event ID cannot reconstruct a discarded payload, and successful enqueue to either bounded channel is not proof of durability.

## Examples and consumer inputs

- **Positive:** both enqueue paths fail for event E. The request already admitted may finish; later inference gets the specified 503. Status reports a non-clear incident and `unrecoverable_lost >= 1` only if E is confirmed discarded; otherwise E is `unknown`.
- **Positive:** database write fails and the monitored journal path accepts E. Fallback use alone leaves incident state clear; healthy pending work is not counted as an incident. If that writer later reports a write/flush failure, latch blocked then. Any still-retained E is incident-attributed `volatile_pending` until durability is established; channel acceptance alone never proves `durable_replayable`.
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

**Unresolved acceptance question:** Keep incidents blocked across restart, but permit an authorized operator to explicitly accept confirmed or irreducible possible loss and resume with a visible `acknowledged` incident? Recommendation: yes. The alternatives are restart clearing the incident, or requiring proven recovery even when uncertainty is irreducible (potentially permanent blocking). This extends the settled forwarding policy; it is not accepted until David/Cos decides.

#484 acceptance dependency: demonstrate affirmative no-incident/reconciled evidence for healthy overlapping replacement while the predecessor is alive; old-process exit cannot be the only proof. The proposed acknowledgement must remain auditable through restart. #485 defines completion acknowledgements; implementation is blocked until both mechanisms are accepted and qualified.
