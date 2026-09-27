# STV-M5-03 proposal: normalized ErrorEvent

**Status: proposal only.** David/Cos must accept this exact artifact before consumers implement it. This file changes no runtime behavior and does not approve retry-policy changes.

## Proposed event

One event records one failure observation. Rust names and types:

| Field | Type | Null / invariant |
|---|---|---|
| `id` | `Uuid` | Required UUIDv7 generated once for each observed failure event. Reuse it for persistence/write retries of that same observation. |
| `occurred_at` | `DateTime<Utc>` | Required UTC timestamp at observation. |
| `request_id` | `Option<RequestId>` | Null only when no logical request was created. |
| `attempt_id` | `Option<AttemptId>` | Null when no upstream attempt exists; if present, `request_id` is present and owns it. |
| `attribution` | `Option<PrincipalAttribution>` | Null means unresolved/legacy; object is the complete trusted Organisation/User/Client UUID tuple from accepted #500. Never infer or accept client-supplied IDs. |
| `category` | closed `ErrorCategory` | Required; values below. |
| `code` | closed `ErrorCode` | Required; only category/code pairs below are valid. |
| `stage` | `Option<ErrorStage>` | Null when the producer cannot identify the stage; never infer a finer phase than observed. |
| `retryable` | `bool` | Required classification hint, not authorization to retry. |
| `upstream_status` | `UpstreamStatusEvidence` | `unobserved`, `no_response`, or `received(u16)` as defined below. |

Use the existing UUID crate and UUIDv7 convention. Distinct observations get distinct IDs. Repeating the same ID with the same event payload is idempotent; the same ID with a different payload is a conflict and must not silently overwrite. Serialize only the fields in the table above; do not add message, raw body, URL, headers, provider-native free strings/codes, prompt, credential, or stack-trace fields. Do not treat current attempt provider/account strings as identity; see the #100 gate below.

`UpstreamStatusEvidence` is a closed enum: `Unobserved` means the source error did not retain whether status headers arrived; `NoResponse` means the client observed failure before receiving a status (it does not prove the provider did not process the POST); `Received(u16)` means the exact upstream HTTP status was observed. A later body-read or decode failure must preserve `Received(status)`, including 2xx, if the caller observed it; if the source error discarded the observed value, emit `Unobserved` rather than reconstructing it. Never substitute Steve's downstream response status.

### Closed taxonomy

Categories: `gateway`, `transport`, `protocol`, `provider_status`, `timeout`, `cancellation`, `overload`.

| Category / code | Stage | Evidence and retryable |
|---|---|---|
| `gateway / invalid_request` | `ingress` | Local request rejection; `NoResponse`; false. |
| `gateway / invalid_configuration` | null unless a request stage is observed | Existing upstream client configuration failure; `NoResponse` when client creation failed before a call; false. |
| `gateway / unsupported_request` | `ingress` | Request selects unsupported streaming mode; `NoResponse`; false. |
| `gateway / service_draining` | `ingress` | Lifecycle rejects work while draining; `NoResponse`; false. |
| `transport / upstream_transport` | nullable upstream stage | Connect/read failure; `NoResponse` only when observed before status, `Unobserved` if provenance was lost, `Received(status)` if body reading failed after headers; false because POST execution may be ambiguous. |
| `protocol / malformed_response` | `response_validation` when observed, otherwise null | Invalid JSON or unexpected content type; `Received(status)` if captured (including 2xx), otherwise `Unobserved`; false. |
| `provider_status / rate_limited` | `upstream_response` | HTTP 429; `Received(429)`; true classification hint. |
| `provider_status / server_error` | `upstream_response` | HTTP 5xx; retain actual status as `Received(status)`; true only for 503, false for other 5xx. |
| `provider_status / rejected` | `upstream_response` | Other non-success HTTP status; retain as `Received(status)`; false. |
| `timeout / upstream_timeout` | nullable upstream stage or `downstream` | `NoResponse` only when observed before status, `Unobserved` if provenance was lost, or `Received(status)` after headers; false. |
| `cancellation / request_cancelled` | nullable upstream stage or `downstream` | `NoResponse` only when observed before status, `Unobserved` if provenance was lost, or `Received(status)` after headers; false. |
| `overload / admission_limit` | `ingress` | Local capacity rejection; `NoResponse`; false. |

Stages are closed: `ingress`, `upstream_connect`, `upstream_response`, `response_validation`, `downstream`. `null` means the source error does not carry enough provenance to select one. The current shared transport/decode variants can arise during both `.send()` and body reads, so map those to null unless the caller supplies the observed boundary; do not claim `upstream_connect` from the error kind alone. `Config` maps to `gateway / invalid_configuration`; `StreamingNotSupported` maps to `gateway / unsupported_request`. An unsupported upstream shape maps to `protocol / malformed_response`; do not create provider-specific codes.

`src/server.rs` checks `lifecycle.enter()` before the Messages/Chat provider call and returns HTTP 503 `{"error":"draining"}` on rejection. Normalize that distinct observation as `gateway / service_draining`, not `overload / admission_limit`.

## Invariants and examples

- `retryable` describes only a potentially transient failure class. It never directs replay: retries require a separate accepted policy and request-safety decision. Never replay after downstream output; a POST with ambiguous upstream execution is not made safe by this field.
- HTTP 429 and 503 normalize identically across adapters to their provider-status codes and preserve only the numeric upstream status. A 2xx status remains evidence when response-body parsing fails. A gateway-generated 502 stays a gateway response and must not become `upstream_status=502`.
- Malformed JSON/content type maps to `protocol / malformed_response`; timeout maps to `timeout / upstream_timeout`; cancellation to `cancellation / request_cancelled`; local admission rejection to `overload / admission_limit`.
- Lifecycle rejection while draining maps to `gateway / service_draining`, separate from capacity rejection `overload / admission_limit`.
- Redaction is allow-list based: serialize only the fields above and the accepted complete attribution tuple. Error display text is derived from category/code, never copied from adapter error strings or response content.
- The architecture spec requires errors to preserve Organisation → User → Client attribution where resolved [§5](../specs/2026-09-26-steve-gateway.md#5-identity-and-attribution). Current `Request`/`RequestAttempt` have no attribution fields; this proposal uses the exact optional tuple only after #500 is accepted. Until then, attribution is null, not fabricated.
- The linked attempt's current provider/account strings include placeholders such as `unassigned`; they are not trusted provider/account identity. Do not expose or aggregate them as identity. Join authoritative provider/account records only after the relevant #100 contract is accepted and the attempt references those records.

## Existing behavior and compatibility boundary

The adapters currently expose parallel typed errors in [`openai_upstream.rs`](../../src/proxy/openai_upstream.rs) and [`anthropic_upstream.rs`](../../src/proxy/anthropic_upstream.rs); current attempts store only `Pending`, `UpstreamError`, `Success`, or `Cancelled` [`types.rs`](../../src/proxy/types.rs). Non-2xx paths retain status only; timeout, transport, JSON, content-type, configuration, unsupported-streaming, and cancellation are distinct internal variants. Chat currently retries one explicit 503 once; other routes do not. This proposal does not change that behavior.

The inspected source has no persisted normalized error history or ErrorEvent table; the generic `steve_background_events` store is not an ErrorEvent log. There are no historical ErrorEvents to backfill. Existing attempt statuses have no category/code evidence: do not synthesize events from them. A legacy/imported record without a verified closed category/code is outside this schema and must be excluded or held for review, never encoded with a null/unknown category or code. Attribution alone may be null. SQLite #181 and PostgreSQL #205 consumers add their own additive storage migrations after acceptance; rollback must preserve existing history and must not invent rows.

## Consumers and gates

Direct consumers listed in [backlog-index.md](../backlog-index.md): #180, #181, #203, #204, #205, #206, #210, #257, #268, #269, #299, and #317. Keep them blocked until this exact artifact revision is accepted; then each owner must update and re-size its brief with the chosen fields, fixture, and runnable command. #299 remains a separate authenticated query-contract decision. #271 owns composed telemetry qualification and stays blocked until its listed leaves and real-process fixture prerequisites are accepted.

**Review evidence:** check each acceptance example against both adapter error enums/callers; verify closed pairs, nullable rules, redaction allow-list, retry boundary, compatibility statement, and consumer handoff. This is documentation-only; no runtime command or test is claimed.
