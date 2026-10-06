# agent-llm adapter spike

Pinned source: `c7ac602ecc8c644ecd8fcee6008b0632e32b5a85` from the official agentgateway source. The original scratch directory contained an isolated git-archive copy; no Steve production files or dependencies were changed.

## Evidence

- Source license: root LICENSE and workspace.package `license = "Apache-2.0"`.
- Source packaging: workspace version `0.0.0`, `publish = false`; agent-llm inherits both. It is not configured as a published standalone dependency at this pin. crates.io API lookup failed and search did not find an exact package; current registry absence was NOT verified.
- Dependencies include local agent-core and agent-http, plus the CEL fork. Root patches pin async-openai, schemars, http-serde, wiremock and yaml-serde-edit to Git revisions. agent-core pulls observability dependencies, so this is broader than a small conversion-only crate.
- Initial offline attempts stopped before compilation on a missing cached async-openai Git revision. The authorized online retry fetched all pinned Git patches and crates into isolated scratch, with `env -i` and isolated HOME/CARGO_HOME/target; no inherited provider credentials. Build completed in 1m06s using rustc1.97.1 (workspace requires1.90). See online-build.log.
- Final scratch Rust verification: three tests passed, zero failures; see online-fixture-verification.log. The third test intercepts tracing locally and confirms a synthetic malformed-response marker is emitted in the raw-body field. It does not forward that event to a logger or print body content. Therefore the no-leak adoption gate is demonstrably failing, despite the regression test passing.
- Pinned upstream golden fixture `golden_tests::responses::completions_to_messages_stream_preserves_cache_usage` compiled and passed (one test;407 filtered out). See upstream-golden-verification.log. No full upstream suite or release certification was attempted. The reproduced adapter fixture is retained beside this report; build logs remain local scratch evidence and are not bundled.
- `rustfmt --edition 2024 --check crates/llm/tests/steve_adapter_spike.rs` passes. No runtime performance measurement was made.
- `python3 check_golden.py` passes pinned snapshot assertions: Anthropic 3883 cache-exclusive input + 30464 cached input converts to OpenAI 34347 cache-inclusive input, output49. This is snapshot inspection only.

## Reusable test artifact

`steve_adapter_spike.rs` uses real `conversion::completions::from_messages::translate_stream` with a `StreamingUsageReporter`, synthetic content, and `LogContentFields::default()`.

It tests cache accounting (100 inclusive input,20 cached,30 cache-write ->50 exclusive wire input), no captured completion/tool messages, one reporter completion on Drop, absent usage remaining `None` rather than zero in telemetry, and confirms malformed-response raw-body logging. All three tests compiled and passed.

Reproduction using the isolated dependency cache:

```
cd /tmp/steve-agent-llm-adapter/upstream
env -i PATH=/opt/homebrew/bin:/usr/bin:/bin HOME=/tmp/steve-agent-llm-adapter/home CARGO_HOME=/tmp/steve-agent-llm-adapter/cargo-home CARGO_TARGET_DIR=/tmp/steve-agent-llm-adapter/target TMPDIR=/tmp cargo test --offline --locked -p agent-llm --test steve_adapter_spike
env -i PATH=/opt/homebrew/bin:/usr/bin:/bin HOME=/tmp/steve-agent-llm-adapter/home CARGO_HOME=/tmp/steve-agent-llm-adapter/cargo-home CARGO_TARGET_DIR=/tmp/steve-agent-llm-adapter/target TMPDIR=/tmp cargo test --offline --locked -p agent-llm completions_to_messages_stream_preserves_cache_usage
```

## Recommendation

Do not add this crate to Steve yet. Preserve the current passthrough release; keep cross-protocol conversion as a separate narrow adapter package whose dependency cost and semantics are independently accepted.

No-leak gate: `crates/llm/src/lib.rs` function `logged_response_parsing` logs up to1024 raw body bytes at WARN. `LogContentFields::default()` disables successful content capture but does not suppress this error path. Before adoption, provide an upstream redaction fix or explicit adapter tracing policy that discards agent-llm parsing events, and execute synthetic-marker tests across malformed JSON, malformed SSE and tool content. Filtering WARN alone would also suppress useful diagnostics; it is not a complete product decision.

Missing usage: retain `Option`/unknown telemetry, never infer free/zero usage from conversion-generated wire zero values. Keep reported counts separate from request-tokenizer estimates (`LLMInfo::input_tokens` can fall back to request estimate), and normalize cache inclusion explicitly by provider convention. Publish final usage on normal EOF, cancellation and Drop through an idempotent reporter; the guard itself has no Drop hook, whereas upstream agentgateway's AmendOnDrop implementation does. Partial usage on disconnect must remain distinguishable from complete final counts. No raw body, prompt, tool arguments or provider credentials belong in the telemetry projection.
