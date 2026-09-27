# STV-M0-09: independent admission budgets

**Acceptance:** proposed while [PR #466](https://github.com/djh00t/steve/pull/466) is open; accepted at that PR's merge commit once David authorizes its merge into `main`. David is the accepting authority. Consumers must record that merge SHA as the accepted artifact revision; they remain blocked until the merge and their brief updates. Later contract revisions require their own acceptance. Verified base: `2f8df419af390ef8cee57f9a0bab144872782b11`, including the provider fixture and protocol disconnect tests from #463–465.

## Evidence and proposed decision

Issue #90 and spec §17 require independent listener budgets. On the reviewed source, `RequestCounters`/`track_requests` observe but do not cap requests; `hold_guard_until_body_end` already holds request-counter guards through response EOF/error/drop; `/api/v1/system/status` exposes current request counts. `ProviderProbeState` separately limits provider-health work to one probe and returns `429 {"status":"busy"}` when that probe is occupied. Preserve this behavior independently from listener admission.

Propose fixed per-process capacities of **32 inference** and **4 management** requests. These are unmeasured starter limits, not throughput targets or an SLA. Use immediate `try_acquire` semantics; do not queue. Do not add config. Revisit only when observed saturation or deployment evidence shows a need. The limits cover active routed requests/responses, not bytes, accepted sockets, or request rate.

On exhausted listener capacity, return **503** with `Content-Type: application/json`, `Cache-Control: no-store`, `Retry-After: 1`, and this exact body (substitute `management` on management listener):

```json
{"error":{"type":"overloaded","code":"admission_limit","message":"inference capacity exhausted"}}
```

`Retry-After` is an advisory delay in seconds. Keep the provider-health probe-gate response as its existing 429 busy response.

Acquire before the handler and hold the permit until response-body EOF, body error, or body drop/disconnect. RAII must release on cancellation/panic. This applies to JSON and SSE; do not release a stream permit when headers are created. Continue to let graceful shutdown and the existing `server.drain_timeout_seconds` control drain. When lifecycle is draining, the inference gate bypasses capacity acquisition and delegates to the existing route handlers, preserving their `503 {"error":"draining"}` response. This does not permit upstream work: their existing lifecycle guards still reject it. Management retains its independent capacity gate during drain. A request already holding a permit releases it normally.

Exempt only `GET /health/live` and `GET /health/ready` on both listeners from admission so they remain observable during saturation. Keep them request-counted under the existing counters. Add the following required, non-null object to both health bodies and `/api/v1/system/status`; preserve all existing fields and meanings:

```json
{
  "admission": {
    "inference": {"limit": 32, "active": 0, "rejected_total": 0},
    "management": {"limit": 4, "active": 0, "rejected_total": 0}
  }
}
```

Exact field contract: `limit` is a required positive JSON integer (u32), fixed at 32/4; `active` is a required JSON integer (u32), the number of currently held admission permits, excluding the two exempt health routes; `rejected_total` is a required JSON integer (u64), incremented once for each non-exempt request rejected by that listener's admission gate. It is monotonic and saturating for that process lifetime, starts at zero on process start, and resets on process restart. There are no null/unknown states. `/api/v1/system/status` consumes a management permit, so its snapshot includes itself when admitted and may itself receive overload at full capacity; the exempt health routes continue to report the snapshot then. Readiness remains lifecycle readiness, not spare-capacity status. Existing `active_*_requests` counters retain their current semantics and may include health requests.

The externally visible changes are additive health/status fields at all loads and fast 503 rejection at saturation. No new config, dependency, persistence, distributed/global cap, per-user fairness, body-size/rate limit, metrics exporter, or accounting/deferred-queue change is proposed.

## Bounded downstream slices

After acceptance, split #91 into the first three implementation leaves below; keep #91 as their coordination parent and #92 as the composed process gate. These are proposed issue boundaries, not dispatch-ready implementation briefs. Before dispatch, record the accepted contract SHA, concrete starting symbols, exact ownership, prerequisites and verified candidate command in each issue. One writer serializes `src/server.rs` changes.

| Slice | Ownership / prerequisite | Observable acceptance and fault |
| --- | --- | --- |
| Inference admission | `src/server.rs` inference middleware and existing body guard; accepted #90 | Wire the 32-slot limit end to end with exact overload response and health exemptions. Use a focused in-process body-lifetime test for retained/released permits and overload response formatting; #92 owns the full real-process 32-stream qualification. Early guard release must fail the held-permit assertion. |
| Management admission | `src/server.rs` management middleware; inference slice merged | Reuse the guard for the independent 4-slot management budget. A held management handler exhausts four slots; the fifth rejects, exempt health and inference remain reachable. A shared semaphore must fail isolation. Keep the existing provider-probe 429 distinct. |
| Admission status | `src/server.rs` health/status responses; both enforcement leaves merged | Extend the existing saturation scenario to assert the required JSON fields, health exemptions and self-counting status request. Missing rejection increments or counting exempt requests as held permits must fail. |
| Multi-stream fixture | `tests/support/upstream.rs` and its existing fixture qualification test; #73 accepted | Current `ControlledUpstream` has one tail gate. Add only the ability needed to hold 32 simultaneous upstream bodies until explicit release, observe all requests, and clean up on failure. A single-response fixture must fail the all-streams-held assertion. This is a new fixture leaf, not reopening accepted #73. |
| #92 real-process gate | `tests/e2e.rs`; all four leaves plus #72 merged | Hold 32 real-daemon inference streams, assert the next request's exact overload response, and query live/ready/status/version while held. Release streams and prove admission resumes. Shared listener capacity must fail management reachability. |

Each leaf targets 5–10 minutes of active implementation after its inputs exist; split further if source inspection shows it cannot fit. The enforcement leaves must leave their listener working at every merge, rather than landing unused guard scaffolding. Tests are added by their owning producer and must select a nonzero count on its candidate. Reuse the held-body scenario for status assertions; no separate duplicate qualification package. The #92 candidate command is `cargo test --test e2e --all-features inference_saturation_keeps_management_live`; it is planned, not currently implemented.
