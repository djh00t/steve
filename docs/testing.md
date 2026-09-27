# Testing Steve

Status: agreed testing policy; the rollout work packages in the backlog implement the missing harness and gates. The commands labelled future below are not available until their producer work package merges.

## What matters

The primary evidence is a small set of end-to-end scenarios that exercise the real Steve daemon, HTTP ingress, routing and upstream transport, persistence and management API together. Express acceptance in plain Given/When/Then examples and use a red-green loop for new behaviour. A feature file or Cucumber dependency is not required: the issue contains the example and the executable test carries the same scenario ID.

Prefer strengthening one useful scenario over adding several overlapping tests. Keep focused tests for rules that need precise examples, especially access control, money, protocol parsing and retry boundaries. Do not require one test per function, line-coverage targets, assertion counts or a global mutation-score target. Existing useful regression tests stay until equivalent or stronger evidence replaces them.

A test earns its place by naming the user-visible failure it detects. A passing test that would also pass with the feature removed is not acceptance evidence. Documentation-only and discovery packages need evidence review, not artificial runtime tests.

## Work-package acceptance

Each package must supply:

1. One small outcome and explicit exclusions.
2. Given/When/Then criteria with exact observable results, including the important negative case where relevant.
3. The shared scenario it extends, the command and expected failure before implementation, and the expected result after implementation.
4. A plausible fault that the assertions must detect. For risk-bearing logic, name a targeted generated mutation or a controlled fault experiment.
5. Evidence at the PR head: command, result, scenario, relevant mutant outcomes and any remaining limitation.

Write the meaningful failing assertion first, verify why it fails, implement the smallest change, and run the relevant scenario. A broken test harness is not the intended red result. A filtered Cargo command must select and execute the named test; an exit-zero run with zero selected tests is not a pass. Use real Rust test names in commands, not display-only scenario IDs. For a bug, reproduce the original failure before changing the code. Do not duplicate the same assertion in a second layer merely to satisfy a checklist.

## End-to-end boundary

The harness starts the built `steve` executable using a temporary configuration, database and object-storage directory, then uses real HTTP clients. It starts a deterministic local upstream and validates requests received there. It reads the management API and persisted records to prove side effects, rather than trusting an internal mock call or log line alone.

Use `env!("CARGO_BIN_EXE_steve")` from Rust integration tests so tests exercise the binary built from the current source, including under mutation. Never point acceptance tests at an already-running developer daemon. Reuse the existing `steve test-upstream` process for static protocol fixtures. Because the Rust crate is binary-only, integration tests do not import its private modules. The controlled upstream lives in `tests/support/upstream.rs`, owned by the fixture package, and records inbound requests, gates the tail and signals body drop. It simulates the external provider boundary; do not expose test-only fault controls on the production CLI.

Each fixture owns ephemeral listeners and temporary data. Readiness uses a bounded observable probe. Streaming tests coordinate first-byte and tail-release events, and assert cancellation before shutting down the fixture. Every success, failure and panic path terminates and reaps child processes. Avoid fixed ports, arbitrary sleeps, real provider credentials and machine-specific state.

Use SQLite/filesystem for the fast default scenario. Repeat representative persistence scenarios against PostgreSQL and an S3-compatible fixture to verify supported backends, without multiplying every scenario across every storage combination. Official SDK smoke tests separately qualify SDK compatibility; raw HTTP tests alone must not be labelled SDK proof. Live-provider qualification is opt-in and reported separately from deterministic CI evidence.

## Shared scenario families

| Scenario | What it proves | Representative fault to detect |
| --- | --- | --- |
| E2E-PROXY | Chat, Responses and Messages JSON/SSE traverse the running daemon; bytes arrive before the upstream tail | Buffer the whole stream, corrupt a delta or forward the wrong model |
| E2E-CANCEL | Disconnect cancels upstream work, releases lifecycle/accounting state, and does not replay after output | Disable cancellation or reopen the retry gate after the first byte |
| E2E-DEFERRED | Slow or saturated history/telemetry does not stall forwarding; critical accounting is reconciled exactly once | Await queue capacity, drop the journal record or replay it twice |
| E2E-LIFECYCLE | Readiness closes on drain; an active stream finishes or the bounded deadline terminates it | Keep accepting new work or skip the active-stream guard |
| E2E-IDENTITY | Different users/clients can use only their permitted accounts; secrets stay off responses and logs | Remove an eligibility filter or redact only the successful path |
| E2E-HISTORY | A session crosses providers; association quality is honest; metadata survives disabled content capture and retention | Merge another user's session, write disabled content or delete retained metadata |
| E2E-ACCOUNTING | Attempts retain usage, price and FX provenance; historical charges survive later price changes | Use current FX, charge the wrong attempt, round at the wrong boundary or convert unknown to zero |
| E2E-ROUTING | Access/capability filters precede deterministic scores; the recorded explanation matches the chosen target | Score a forbidden account or select a stale/unrecorded snapshot |
| E2E-TELEMETRY | Attempt timings/errors remain attributable and queryable while a failed exporter cannot stall service | Mix attempt spans or await the exporter on the hot path |
| E2E-COMPOSED | One request joins authenticated identity, permitted account, routing snapshot, canonical session and persisted charge/price/FX provenance | Cross-wire an identity, snapshot or charge reference while isolated area tests still pass |
| E2E-CONTROL | A UI command uses the management API and shows the resulting daemon state and useful errors | Update only local UI state or hide a rejected operation |
| E2E-UPGRADE | Verified replacement becomes ready before cutover and old streams drain; failed replacement rolls back | Cut over before readiness or terminate the serving worker too early |

These are scenario families, not a quota or a request to implement every combination. The owning work package names the concrete test, inputs and assertions. One scenario may prove several packages; the milestone acceptance issue checks the composed journey across them.

## Mutation testing

Use targeted mutation testing to challenge assertions around changed behaviour.
`cargo-mutants 27.1.0` is the qualified Rust runner, selected through Dependency
Advisor's conservative Rust policy (minimum release age 720 hours). Install it
only as an isolated test tool, not a runtime dependency. See the
[verified commands and named-fault evidence](mutation-qualification.md), including
the reviewed buffering patch and the distinction between endpoint E2E and the
shared replay-gate contract test. Official references: [outcome definitions](https://mutants.rs/using-results.html)
and [timeout guidance](https://mutants.rs/timeouts.html).

First run a reliable unmodified baseline. Select only the changed risk-bearing
functions and the assertions that should detect the named faults. Use disposable
copies, bounded concurrency and timeouts, and retain outcomes with the tested
SHA. Never mutate the shared checkout or a deployed service. A crucial fault the
runner cannot generate may use one reviewed disposable patch with evidence that
the intended assertion fails.

Triage outcomes honestly:

- **Caught:** a relevant assertion failed because the behaviour was wrong.
- **Missed/survived:** examine the invariant; strengthen an existing test when the changed behaviour matters.
- **Equivalent:** document why no observable contract changes; review the exclusion.
- **Unviable:** the mutant does not compile; this is not test-strength evidence.
- **Timeout/infrastructure failure:** investigate separately; do not count it as a useful catch or hide it by increasing timeouts without evidence.

No unresolved non-equivalent survivor may undermine the package's stated acceptance invariant. An unrelated surviving mutant does not justify expanding a small package into an exhaustive suite; record a concrete follow-up if it represents a real risk. Exclusions must be specific and explained, not broad skips to make a percentage green.

## Commands and gates

Currently available: `make check` checks formatting, clippy and compilation; `make test` runs the current Cargo tests. The pre-commit and pre-push hooks both run `make check`; contributors also run the affected acceptance scenario. Current CI still runs the legacy broad gate on push and PR. The backlog includes the workflow change needed to align CI with the following target:

To qualify the official Anthropic and OpenAI Python SDKs against the local deterministic fixture, install the pinned SDKs in a virtual environment, build Steve, then run `python scripts/sdk_smoke.py` with that environment's Python. The default `--provider anthropic` preserves the published command; use `--provider openai` for OpenAI or `--provider all` for both. For example: `python3 -m venv target/sdk-venv && target/sdk-venv/bin/python -m pip install -r tests/requirements-sdk.txt && cargo build && target/sdk-venv/bin/python scripts/sdk_smoke.py --provider all`. The harness uses temporary SQLite/filesystem state and a dummy local API key; it does not contact a live provider. Anthropic 1.2.0 and OpenAI 3.6.0 were approved by Dependency Advisor under the conservative Python policy (720-hour minimum release age). It exercises the official SDK streaming helpers and their accumulated final results for Chat Completions, Responses, and Messages. The fixtures follow the [official Messages event sequence](https://platform.claude.com/docs/en/build-with-claude/streaming) and [Responses streaming events](https://developers.openai.com/api/docs/guides/streaming-responses). Hosted SDK execution is tracked separately in #83; these commands are the local SDK qualification.

- During development: run the smallest meaningful failing/passing scenario and changed-scope `make check`.
- Before committing/pushing: `make check` plus the affected acceptance scenario. Do not run local `make quality-gates` or `make check-full`; broad quality/release gates belong to post-merge `main` CI.
- PR CI: affected meaningful E2E scenarios, required platform compilation and targeted mutation for changed high-risk logic. If impact cannot be determined safely, run the complete deterministic E2E set.
- Main CI: the full deterministic integration set, representative backend parity, release/container gates and the broader mutation batch. Report every failure; main CI is not a substitute for proving a PR's changed behaviour.

A consumer cannot claim a dependency-provided command is runnable until its producer has merged and the command has been run on the candidate base. A producer proves its own new command on its candidate branch. Test runtime, flakiness and mutation usefulness should inform later gate tuning; do not invent a performance budget before measurement.
