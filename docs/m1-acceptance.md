# STV-M1-04: Chat terminal-attempt accounting event

This contract consumes [STV-M0-16](contracts/stv-m0-16.md) at the explicitly accepted [PR #567 head `7d2a54b03394be8d71c2018b73286e7a08a64cd5`](https://github.com/djh00t/steve/issues/483#issuecomment-5885737021). That acceptance covers incident/admission policy, not runtime recovery qualification. This note defines the event offered to `DeferredQueues::accounting` by [#95](https://github.com/djh00t/steve/issues/95) and [#96](https://github.com/djh00t/steve/issues/96); it does not implement emission, persistence, or the #484/#485 recovery mechanisms.

## Shape and identity

Use the existing `AccountingEvent` envelope, with exactly one kind for Chat Completions: `chat.attempt.terminal.v1`. All fields below are required. UUIDs are canonical strings; timestamps are RFC 3339 UTC strings. `null` means attribution was not resolved, never an empty string or the current `unassigned` placeholder.

| Location | Field | Type and meaning |
| --- | --- | --- |
| Envelope | `id` | UUID string generated once for this event; retain it unchanged through queue, journal and replay. It is the stored event ID, not proof of storage. |
| Envelope | `kind` | Literal `chat.attempt.terminal.v1`. |
| Envelope | `created_at` | Time the terminal event was created; it is not the request or attempt start time. |
| Envelope | `payload` | Object with the fields below. |
| Payload | `request_id` | UUID string of the logical `Request`. |
| Payload | `attempt_id` | UUID string of the terminal `RequestAttempt`; stable semantic key for this kind. |
| Payload | `request_created_at` | `Request.created_at`. |
| Payload | `model` | Nonempty requested Chat model string, as validated at ingress. |
| Payload | `provider`, `account` | Each a nonempty string or `null` when unresolved; do not invent attribution. |
| Payload | `status` | One of `success`, `upstream_error`, `cancelled`, matching terminal `AttemptStatus`. Never `pending`. |
| Payload | `started_at`, `finished_at` | `RequestAttempt` times; `finished_at` is non-null for every emitted event. |

For a given `(kind, attempt_id)`, offer one event on the first terminal transition. A retry creates a new `attempt_id` under the same `request_id` and gets its own event, including a failed first attempt followed by success; do not emit one aggregate event per request. Do not regenerate an event ID for a retry of the *same accounting delivery*: replay the same envelope. A second terminal callback or a body drop after EOF/error must not produce another event for that attempt. Invalid requests, rejected admission, and an unconfigured upstream stub have no terminal upstream attempt to account for.

Nonstream success or final upstream failure emits after its attempt is terminal; each earlier retried attempt also emits. SSE uses the identical kind, fields and cardinality: emit after EOF (`success`), upstream error (`upstream_error`), or client cancellation/disconnect (`cancelled`), including a failure before response headers. Never emit while the stream is `pending`; a later body drop cannot overwrite a prior terminal outcome.

## Incident boundary

Offer the event without waiting for the database worker. Queue or journal-channel acceptance is not a durable write, replay acknowledgement, or proof of recoverability. Under accepted STV-M0-16, primary-queue **and** journal failure (including later database-write failure followed by journal failure), or journal write/flush failure, latches the accounting incident when the serving process observes it. An already admitted Chat request may finish forwarding; inference whose admission linearizes after the local latch gets `503 accounting_incident` while state is `blocked` or `unreconciled`. Successful fallback alone does not latch an incident. Event identity never reconstructs a discarded payload. #484 owns logical ownership and restart/overlap evidence; #485 owns replay, disposition and resumption evidence. This contract makes no recovery-completion claim.

## Review examples

**Positive:** one nonstream 503 attempt is retried successfully. Two events share `request_id`, have distinct `attempt_id` and envelope `id`, and carry `upstream_error` then `success`.

```json
{"id":"01995200-0000-7000-8000-000000000002","kind":"chat.attempt.terminal.v1","created_at":"2026-09-27T08:00:02Z","payload":{"request_id":"01995200-0000-7000-8000-000000000003","attempt_id":"01995200-0000-7000-8000-000000000004","request_created_at":"2026-09-27T08:00:00Z","model":"gpt-test","provider":"openai","account":null,"status":"upstream_error","started_at":"2026-09-27T08:00:00Z","finished_at":"2026-09-27T08:00:01Z"}}
```

**Positive:** an SSE attempt stays absent from accounting while pending; disconnect creates one `cancelled` event with non-null `finished_at`.

**Negative:** enqueue at SSE header time emits `pending`, or EOF followed by body drop emits a second `cancelled` event. Both violate terminal-only, one-per-attempt cardinality.

**Negative:** a full primary queue and failed journal enqueue are treated as durable because an `id` exists, or a later request is admitted after the incident latch. Both violate STV-M0-16.

## Consumer handoff

- [#95](https://github.com/djh00t/steve/issues/95) owns nonstream emission from `ChatCompletionReply::attempts` through the existing synchronous `DeferredQueues::accounting` call. Its `chat_nonstream_accounting` process test must inspect stored envelope/payload, both retry attempts, and one row per emitted event ID without waiting on storage in the response path.
- [#96](https://github.com/djh00t/steve/issues/96) owns SSE terminal emission, including before-header upstream failure, EOF, body error and disconnect. Its `chat_stream_terminal_accounting` process test must prove no early event, one final event with the correct status, and no second event after a terminal outcome.
- Neither consumer may claim the incident gate, replay, restart recovery, or operator disposition works until #484/#485 and their runtime qualification are complete.
