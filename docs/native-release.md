# Native single-user release candidate

This release boundary is an opt-in native daemon for one operator: OpenAI Chat,
Responses and Anthropic Messages JSON/SSE proxying, `/v1/models`, basic metadata
and usage telemetry, centrally configured upstream credentials, and deterministic
least-cost routing. Existing accounting integrity, drain, cancellation and
no-replay rules remain in force. The larger multi-user platform is later work.

## Install and configure

Build this exact candidate with `cargo build --locked --release --all-features`.
Run `target/release/steve --version` to identify the binary version; the candidate
handoff records its source SHA and SHA-256 separately.

Copy `config.native.example.toml` to a private local configuration. Set both
listeners to numeric loopback addresses, an absolute private accounting root,
and supported upstream model names and prices. Create the SQLite parent and
object-store directories before starting. The example prices are illustrative,
not provider quotes. Configure an Anthropic provider with `protocol = "anthropic"`
and models declaring `protocols = ["messages"]`; OpenAI providers support `chat`
and `responses`. Configured compatible providers can coexist.

Credentials are referenced by environment-variable name in `credential_env`;
never put the value in the configuration, URL, command arguments or logs. Use
your existing secret manager to supply that variable to the process. For a
manual Bash session, hidden input avoids shell history containing the value:

```bash
read -r -s -p 'Upstream API key: ' STEVE_NATIVE_OPENAI_KEY
export STEVE_NATIVE_OPENAI_KEY
# Use your chosen absolute accounting root in both config and provisioning.
STEVE_OPERATOR=local-operator target/release/steve accounting provision --root /absolute/private/steve/accounting
target/release/steve --config /absolute/private/steve/native.toml serve
unset STEVE_NATIVE_OPENAI_KEY
```

Environment credentials are in process memory/environment; this release does
not provide an OS keychain service. Missing, empty or invalid enabled-provider
credentials prevent startup. Disabled providers do not require a credential.
Changes to models, prices, URLs, CA bundles or credentials require a restart.
Do not enable HTTP/header/payload debug logging in the process supervisor.

## Client boundary and routes

Native mode requires both inference and management listeners on loopback.
Local OS users/processes can access these endpoints; it does not authenticate
local clients or isolate mutually untrusted local users. Do not expose or tunnel
these listeners. Client access authentication is separate future work.
Client-supplied provider authorization does not grant access and is not
forwarded: the selected upstream receives only its configured server credential.

`GET /v1/models` lists enabled configured models with an enabled provider.
Send a public model ID to select that configured route, or `model = "auto"` for
least-cost selection among enabled routes declaring that endpoint protocol and
streaming capability. The upstream receives `upstream_model`, not the alias.
Unknown IDs and unavailable/incompatible routes fail explicitly; no silent
fallback, protocol translation, or replay after streamed output is added.

Prices are nonnegative integer USD micro-units per million input/output tokens
(1,000,000 micro-units = USD 1). Zero explicitly means free; missing prices are
invalid. Auto compares `input_price * reference_input_tokens + output_price *
reference_output_tokens` using integer arithmetic; ties use public model ID.
The reference-token counts are configured estimates, not prompt tokenization or
a guarantee of the cheapest eventual invoice. Update prices/capabilities yourself.
Eligibility is configured enablement plus protocol/stream support, not a live
provider model/health/quota check. Tools, modality and context-window constraints
are not inferred; expose only models you have verified for your client workload.

```bash
curl http://127.0.0.1:11435/v1/models
curl http://127.0.0.1:11435/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"auto","messages":[{"role":"user","content":"hello"}],"stream":true}'
```

## Telemetry and acceptance

Structured telemetry reports request correlation, public model/provider,
HTTP status, response bytes, first-byte and completion latency and terminal
completion/error/cancellation. Existing attempt/accounting records remain.
Token usage is captured when the JSON or SSE response supplies recognizable
usage; missing usage remains unknown. Telemetry does not fabricate token counts,
log prompts/response content or convert configured prices into authoritative
billing charges. Parser retention is capped at 64 KiB per JSON response or SSE
event; oversized payloads/events omit that usage and report it as unknown.
Forwarding stays streaming and raw response bytes are unchanged.

The candidate acceptance check is `cargo test --locked --all-features --test
e2e_native`, plus native module tests and existing streaming/accounting regressions.
Deterministic upstream tests establish credential replacement and route behavior;
they do not establish real-provider acceptance. A real-provider smoke needs an
operator-authorized provider/model, supplied credential and approved small request
budget. Review the exact candidate, basic response, stream and telemetry before
calling real-provider use accepted.

Deferred: account pools, household/enterprise identities and RBAC, full histories,
pricing/FX ledgers, advanced health/quota routing, TUI, Swift controller and new
platform deployment guarantees. This candidate does not claim new Windows,
OS-crash or power-loss durability support. Existing documented limits apply.
Rollback is stopping this candidate and using the prior binary/config; do not
remove accounting state or use rollback to bypass unresolved incidents.
