# STV-M1-04: Chat terminal-attempt accounting event

This contract uses [STV-M0-16](contracts/stv-m0-16.md) at [PR #567 head `7d2a54b03394be8d71c2018b73286e7a08a64cd5`](https://github.com/djh00t/steve/issues/483#issuecomment-5885737021), [STV-M0-17](contracts/stv-m0-17.md) at [PR #568 head `48fc33f36d7a147db7373d30c9a5653749895f12`](https://github.com/djh00t/steve/issues/484#issuecomment-5885739296), and [STV-M0-18](contracts/stv-m0-18.md) as working design baselines. Final operator and release acceptance occurs once at the combined touchable M1 gate. This note defines the event offered to `DeferredQueues::accounting` by [#95](https://github.com/djh00t/steve/issues/95) and [#96](https://github.com/djh00t/steve/issues/96); it does not claim emission, persistence, recovery, or platform qualification is complete.

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

Offer the event without waiting for the database worker. Queue or journal-channel acceptance is not a durable write, replay acknowledgement, or proof of recoverability. Under the STV-M0-16 working design baseline, primary-queue **and** journal failure (including later database-write failure followed by journal failure), or journal write/flush failure, latches the accounting incident when the serving process observes it. An already admitted Chat request may finish forwarding; inference whose admission linearizes after the local latch gets `503 accounting_incident` while state is `blocked` or `unreconciled`. Successful fallback alone does not latch an incident. Event identity never reconstructs a discarded payload. #484 owns logical ownership and restart/overlap evidence; #485 owns replay, disposition and resumption evidence. This contract makes no recovery-completion claim.

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

## Integrated implementation and qualification handoff

All commands below are reserved selectors for `tests/e2e_accounting.rs` except #95's existing `tests/e2e.rs` selector. They are not passing evidence until the named target exists, exactly one test is selected, and the command passes on the candidate SHA.

| Owner | File and exact scenarios |
| --- | --- |
| #84 | Coordination only: STV-M0-16/17/18 and the consumer handoff; no independent runtime selector. |
| #85 | `tests/e2e_accounting.rs`: `cargo test --all-features --test e2e_accounting journal_partial_tail_recovery -- --exact --nocapture` |
| #86 | `tests/e2e_accounting.rs`: `cargo test --all-features --test e2e_accounting accounting_reconciles_after_db_recovery -- --exact --nocapture`; owns configured per-operation timeout, total retry deadline, retry interval, exhaustion result, and startup/manual recovery trigger. |
| #87 | `tests/e2e_accounting.rs`: `cargo test --all-features --test e2e_accounting journal_partial_commit_replay -- --exact --nocapture`; `STEVE_TEST_POSTGRES_URL="${STEVE_TEST_POSTGRES_URL:?set isolated test Postgres DSN}" cargo test --all-features --test e2e_accounting postgres_replay_detects_conflicting_duplicate -- --exact --nocapture` |
| #94 | This event contract only; runtime emission belongs to #95/#96. |
| #95 | `tests/e2e.rs`: `cargo test --all-features --test e2e chat_nonstream_accounting -- --exact --nocapture` |
| #96 | `tests/e2e_accounting.rs`: `cargo test --all-features --test e2e_accounting chat_stream_terminal_accounting -- --exact --nocapture` |
| #97 | `tests/e2e_accounting.rs`: `cargo test --all-features --test e2e_accounting chat_accounting_does_not_delay_response -- --exact --nocapture` |
| #483/#485 | `tests/e2e_accounting.rs`: `cargo test --all-features --test e2e_accounting accounting_incident_preserves_forwarding_and_restart_evidence -- --exact --nocapture`; `cargo test --all-features --test e2e_accounting accounting_disposition_is_auditable_and_publicly_redacted -- --exact --nocapture` |
| #484/#485 | `tests/e2e_accounting.rs`: `cargo test --all-features --test e2e_accounting fresh_provision_and_missing_evidence_fail_closed -- --exact --nocapture`; `cargo test --all-features --test e2e_accounting same_replica_replacement_preserves_accounting_ownership -- --exact --nocapture`; `cargo test --all-features --test e2e_accounting legacy_journal_adoption_is_offline_and_resumable -- --exact --nocapture`. A nonempty legacy source remains staged and startup-blocking until Task 3 replay acknowledgement; an empty source may complete vacuously. |
| #485 | `tests/e2e_accounting.rs`: `cargo test --all-features --test e2e_accounting accounting_shutdown_barriers_complete -- --exact --nocapture`; `cargo test --all-features --test e2e_accounting accounting_shutdown_timeout_survives_restart -- --exact --nocapture` |

[#45](https://github.com/djh00t/steve/issues/45) closes only after #94-#97 pass as one deferred-accounting path. [#37](https://github.com/djh00t/steve/issues/37) and [#98](https://github.com/djh00t/steve/issues/98) own the composed browser gate over those results plus the existing Chat, Responses, Messages, cancellation, models, health, official SDK, hosted CI, mutation, and target-platform evidence. They do not create another accounting implementation target or another operator approval step.
