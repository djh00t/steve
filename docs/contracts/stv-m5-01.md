# STV-M5-01 latency contract proposal

Source: [gateway spec, latency instrumentation](../specs/2026-09-26-steve-gateway.md#9-latency-instrumentation) and [M5 plan](../plans/2026-09-26-steve-mvp.md#m5--deep-latencyerror-telemetry), inspected against main `4472ca59896465fcf27b0d1df1d5218552d80efd`.

**Acceptance:** David/Cos reviews Decisions A and B separately at an exact revision. Neither is accepted by publication or merge. Measurement consumers require accepted A; percentile consumers require accepted B and a compatible accepted input shape from A. No runtime or persistence behavior changes here.

## Decision A: latency span and stage vocabulary

**Status:** Proposal only; no API, schema, or migration is accepted. David/Cos acceptance is pending.

### Contract v1

`LatencySpan` records one request or attempt stage. `stage` is a closed, versioned snake_case enum; adding/renaming a value requires a contract version change. Emit at most one span for each `(request_id, attempt_id, stage)`; that tuple is the stable deduplication key (attempt_id is null for request stages).

| Field | Type | Rule |
|---|---|---|
| `contract_version` | `u16` | Required; `1` for this vocabulary. |
| `request_id` | existing `RequestId` (UUID) | Required. |
| `attempt_id` | `Option<AttemptId>` (UUID) | Null only for request-owned stages; required for attempt-owned stages. |
| `stage` | v1 closed enum below | Required. |
| `status` | `completed \| incomplete` | Required; completed means the stage reached its successful normal boundary. Failure or cancellation is incomplete, even when its observation time is known. |
| `start_offset_ns` | `u64` | Required elapsed monotonic time from logical request entry to stage start; capture a shared request monotonic origin. |
| `duration_ns` | `u64` | Required elapsed monotonic time from stage start to normal end or final observation; zero is valid only when actually measured. |
| `observed_at` | UTC timestamp | Required wall-clock timestamp of that same end/final observation; attribution and ordering only, never duration arithmetic. |

No row means the stage did not run or could not be observed; it is unknown, never an implicit zero. An incomplete row has measured elapsed time through its final observation and is excluded from Decision B samples. Do not synthesize a start, offset, end, or duration from wall-clock fields or backfill when the request monotonic origin is unavailable.

### Stage ownership and boundaries

Request-owned (`attempt_id = null`):

- `ingress`: HTTP request enters Steve's instrumented request boundary → complete request body is available to routing. This is not client network arrival time.
- `authentication`: auth check starts → allow/deny decision is made.
- `client_resolution`: client identity lookup starts → resolved or unresolved result is known.
- `user_resolution`: user identity lookup starts → resolved or unresolved result is known.
- `session_resolution`: session lookup starts → resolved, absent, or failed result is known.
- `model_routing`: model routing begins → selected model/route or no-route result is known.
- `request_total`: instrumented request entry → downstream response body completes normally; failure/cancellation has an observed end but is incomplete. This can overlap persistence.

Attempt-owned (`attempt_id` required; create the attempt identity before selection):

- `account_selection`: provider/account selection for this attempt begins → selection or no-selection result is known.
- `request_transformation`: this attempt's transform begins → transformed request is ready for dispatch.
- `response_transformation`: this attempt's response transform begins → final transformed response/body completes; absent if no transform runs.
- `persistence`: this attempt's durable accounting write begins → durable commit is observed; failure/cancellation is incomplete. Queue admission alone is not persistence completion.
- `upstream_connection`: connector begins a new transport establishment → connection/transport is established. Emit only when an actual new dial/transport is observed; pooled connection reuse means absent, and unavailable connector timing means unknown/absent, never send/header latency.
- `response_headers`: reqwest send begins → response headers become available. This cumulative interval overlaps connection and other request-to-response timings; durations are not additive.
- `first_byte`: reqwest send begins → first non-empty upstream body byte is observed by Steve; not a packet-level timestamp.
- `first_token`: reqwest send begins → first complete provider token is identified by a protocol-aware decoder. A byte chunk is not a token; until decoding exists this stage is absent/unknown.
- `final_token`: reqwest send begins → protocol signals the final complete token. EOF alone is not a token signal; without protocol-aware decoding this stage is absent/unknown.

### Attribution and current source limits

Use existing `RequestId`/`AttemptId` UUID wrappers and join spans to their request/attempt records for consumer dimensions. Populate user/client attribution only from accepted identity contract #500; provider/account attribution only from accepted account contract #100. Never write `unassigned`, caller-supplied identity, or guessed IDs as dimensions; unresolved attribution stays null/unknown. Model aggregation also requires its accepted entity ID.

Current handlers accept already-buffered `Bytes`; they cannot recover network ingress/body-upload time, and create `RequestId` only after parsing. Future instrumentation must assign the ID and monotonic origin once at request entry; never fabricate prior IDs or offsets. Current reqwest `send()` combines connection, request-send and response-header wait; exact dial timing is unavailable. Existing wall-clock attempt start/end cannot provide monotonic durations. Streams expose byte chunks, not tokens. Accounting queue submission does not prove persistence commit. These are implementation/source limitations, not permission to report proxies as exact measurements.

Acceptance of this proposal still requires David/Cos approval, an executable qualification fixture/command, and re-sizing the blocked implementation issues against the accepted artifact. Aggregation windows and quantile method belong to independent Decision B; EWMA semantics belong to #175.

## Decision B: percentiles and windows

**Status:** Proposal only; David/Cos acceptance pending. No existing percentile/window policy was found in the cited spec/plan.

- Population means recorded spans with `status=completed` and present `duration_ns: u64` and UTC end `observed_at`; this describes recorded data, not proof that every request/stage was recorded. Incomplete/absent stages are excluded, never zero or discarded eligible samples.
- Each aggregate key is one accepted provider ID, account ID, or model ID plus stage. A region-qualified key is valid only with authoritative accepted region metadata; otherwise that result is unknown. Do not group by display names or guessed regions.
- Proposed windows: short = trailing 60 s; medium = trailing 15 min. Capture `now` once; membership is `[now-window, now)` by `observed_at` (start inclusive, end exclusive). `duration_ns` is measured monotonically; wall-clock adjustment may move membership but cannot alter duration.
- Use one consistent raw-span query snapshot for population and coverage. Span storage (#178/#199 owner) must supply verified coverage bounds covering the full `[now-window, now)` interval and a no-gap/known-loss=false signal.
- Missing, unverified, too-short, gapped, or known-loss coverage means `incomplete_coverage` and unknown results. This is not implemented or guaranteed by this proposal; until the source supplies that evidence, consumers must qualify results as unknown.
- Proposed retention: preserve at least 15 min of raw spans for full medium-window statistics, subject to the user's configured retention. Do not override shorter retention; affected windows return unknown when coverage cannot be proven.
- For verified coverage, sort eligible durations ascending. Nearest rank is integer `rank_p(n)=(p*n+99)//100` (1-based), with no interpolation or bucket approximation. Proposed minima are independently p50 `n>=5`, p95 `n>=20`.
- Numeric example: `[10,20,30,40,100] ns` gives raw p50 rank `(50*5+99)//100=3`, value `30 ns`; p50 is available, p95 is unknown because `n=5<20` (raw p95 rank 5, value 100 ns).
- Numeric example: `[1..20] ns` gives p50 rank 10/value `10 ns`, p95 rank 19/value `19 ns`; with `[1..19]`, raw p95 rank 19/value `19 ns` but p95 remains unknown because `n<20`.
- Internal calculation result only (not an API/schema choice): `p50_ns: Option<u64>`, `p95_ns: Option<u64>`, `sample_count: Option<u64>`, and per-quantile statuses `available | no_samples | below_minimum | overflow | incomplete_coverage`.
- For verified coverage and `n<=10,000`, `sample_count=Some(n)` exactly; `n=0` gives null values/status `no_samples`; below-threshold quantiles are null/status `below_minimum`. Fetch up to 10,001 eligible rows to detect overflow.
- Coverage failure takes precedence over row overflow: incomplete/unverified coverage yields null count and values with `incomplete_coverage`, even if 10,001 rows were read. With verified coverage, `n>10,000` yields null count and values with `overflow`.
- Never silently truncate/discard eligible completed rows or present partial percentiles as complete. No persistent aggregate cache or key-eviction policy is proposed.
- Scope: EWMA remains #175; no OTEL backend or SLO policy is selected. Consumers #182 and percentile query-row consumers remain blocked pending accepted contract, coverage evidence, and re-sizing; no runtime evidence is claimed.

**Open for acceptance:** proposed `60 s / 15 min`, `5 / 20` minima, `10,000` row cap, nearest-rank rule, coverage evidence contract, and `>=15 min` retention target need David/Cos approval or revision; none is claimed as existing policy.

## Consumer handoff and qualification

Direct #174 consumers in the [backlog index](../backlog-index.md): #178, #179, #182, #184, #199, #200, #201, #202, #245, #252, #253, #254, #263, #264, #265, #266, #267 and #270. Their original prerequisites remain. Shared migration and registration files retain one writer. Each brief must reference accepted sections, actual source seams, executable fixture/command evidence and a fresh 5–10 minute scope before dispatch.

Qualification must distinguish absent/incomplete spans from measured zero, preserve timeline offsets across wall-clock adjustment, reject unsupported stage versions, avoid duplicate span samples, and demonstrate window edges, percentile ranks, sample thresholds and overflow. These are required future checks, not passing tests claimed by this proposal. SQLite/PostgreSQL span migration owners #178/#199 must define additive storage and rollback preserving existing history; do not backfill invented timing from legacy timestamps. #182 owns finite-sample percentile implementation; query consumers #246/#248/#249 retain their own prerequisites. EWMA #175 and OTEL #177 remain separate.
