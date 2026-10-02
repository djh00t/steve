# AgentGateway fixture experiment

This is a reversible spike, not a production integration or release approval.
The working Mac app, its config/database, forwarding code, credentials and ledger
are unchanged. No live models were called. The proposed review destination is
`codex/release-0.2.0`; that branch is not a published release.

## Current architecture and recommendation

Steve uses Rust (Axum/Reqwest/Tokio) for forwarding and Swift/AppKit for the native
controller. It has no TypeScript manifest or Effect dependency. Keep Rust in the
latency-sensitive path and retain Steve's routing prices, credential boundary,
admission, cancellation and accounting identity.

Ranked decision:

1. Reuse pinned golden fixtures and evaluate a small conversion/usage adapter.
   A compiled adapter and a live synthetic sidecar conversion both demonstrate
   functionality beyond Steve's native same-protocol forwarding.
2. Keep the direct Steve path as the default. The extra gateway process is useful
   for explicit conversion needs, but these measurements show extra overhead and
   missing-usage compatibility limits, not a performance win.
3. Avoid adding the entire main-only `agent-llm` dependency today. It is workspace
   version `0.0.0`, `publish=false`, depends on local core/http/CEL crates and five
   patched Git dependencies, and has a confirmed raw-response logging path.
   Do not treat gateway budgets as Steve's strict accounting ledger or caps.
4. Consider Effect only for future asynchronous control workflows that warrant
   it. A shared web UI can reuse the existing Rust management HTTP boundary.
   Browser views, typed orchestration and remote authentication/TLS are separate
   design work; the Mac app can remain the launcher. Do not add a Node runtime to
   the forwarding path or build a web UI as part of this spike.

## Pins and license boundaries

- Steve source base: `1f3e37bb59c76e6bf56670b87460d4e2da547e5d`.
- Native gateway: official `v1.6.0-rc.1`, source
  `e951d942e638326bef679fd0132884fb4169820e`, darwin-arm64 SHA256
  `8199e0f2ba333624bbbd75f1983892bb754fbdbcf2f95bfe62a2bc17f1fb7092`.
- Stable `v1.5.0`, source `fe6732474a96a0363dfb9822859af4e9bab360fa`, was
  inspected but not executed: its native listener configuration lacks an explicit
  loopback bind. Its binary SHA256 was verified as
  `da432d35bd696da0564f7b2b6bbc783542b6b9c616d6c0c4d4c3daef9dfa11a1`.
  Do not generalize prerelease findings to a stable production release.
- Adapter source: `c7ac602ecc8c644ecd8fcee6008b0632e32b5a85` from
  <https://github.com/agentgateway/agentgateway>. AgentGateway root/agent-llm are
  Apache-2.0; `crates/pool/LICENSE` is an MIT subcomponent. This experiment does
  not redistribute upstream source, libraries or binaries. Its adapter test is
  new synthetic test code extending upstream conversion coverage; retain the
  upstream source checkout's license when reproducing. Production extraction or
  binary distribution requires exact imported-dependency/license review and
  retained attribution/change notices, not just the root-license assumption.

## Reproduce on macOS arm64

Use an isolated checkout. All child runtime environments are explicitly reduced
and use fake fixture credentials; no environment key is forwarded.

```sh
cargo build --locked
mkdir -p /tmp/steve-gateway-tools
curl -fL https://github.com/agentgateway/agentgateway/releases/download/v1.6.0-rc.1/agentgateway-darwin-arm64 -o /tmp/steve-gateway-tools/agentgateway
shasum -a 256 /tmp/steve-gateway-tools/agentgateway
# Stop unless the checksum matches the pinned value above.
chmod +x /tmp/steve-gateway-tools/agentgateway
python3 experiments/agentgateway/probe.py \
  --steve "$PWD/target/debug/steve" \
  --gateway /tmp/steve-gateway-tools/agentgateway \
  --output /tmp/steve-gateway-results.json
```

The two timeout fixtures each take about 30 seconds. The runner uses separate
loopback ports and temporary SQLite/accounting roots, stops only its children,
and removes fixture state. It creates no service or persistent key. JSON output
records script/binary hashes, platform, request metadata and measured results;
no request/response content or credential value is retained.

For the pinned adapter:

```sh
git clone https://github.com/agentgateway/agentgateway.git /tmp/steve-agentgateway-adapter
cd /tmp/steve-agentgateway-adapter
git checkout --detach c7ac602ecc8c644ecd8fcee6008b0632e32b5a85
# Copy this experiment's adapter/steve_adapter_spike.rs into crates/llm/tests/.
cargo test --locked -p agent-llm --test steve_adapter_spike
cargo test --locked -p agent-llm completions_to_messages_stream_preserves_cache_usage
```

The adapter test performs no network/provider requests. Dependency downloads may
be necessary. Three custom compiled tests and the selected upstream golden test
passed. See [adapter details](adapter/REPORT.md): missing usage remains `None`;
cache-inclusive 100 minus 20 cached and 30 cache-write becomes 50 exclusive;
reporter Drop finalizes once; normal completion/tool content is not captured.
A tracing interception test proves malformed raw-response logging without writing
that intercepted body to a logger. This is a passing regression test proving an
adoption blocker, not successful no-leak acceptance.

## Acceptance and limits

The probe compares the same Steve binary and fixtures directly and through the
pinned gateway. Default gateway retries/failover/budgets are not configured;
Steve alone owns its existing pre-output 503 retry. Both paths produce two
upstream attempts for that case and one for timeout/malformed/disconnect. No
request is replayed after streaming output in the tested disconnect cases.

Normal Chat/Responses/Messages JSON/SSE return 200 and expected text. A 300ms
upstream SSE gap is retained while first body bytes arrive before EOF. Closing
a stream propagates to the fixture for all three protocols. Client-supplied
provider auth is replaced with the intended fake upstream credential. Normal
same-protocol unknown request JSON is retained; conversion may discard fields
without a corresponding target-schema meaning.

Chat missing usage stays unknown (gateway emits `usage:null` rather than omitting
the key); Responses missing usage stays unknown. Messages missing usage passes
through Steve but yields 502 through the gateway. This deliberately incomplete
provider fixture demonstrates a compatibility limit, not proof that a valid
provider response fails. Adapter wire defaults must never turn unknown usage
into a zero-priced ledger entry.

Both paths measure 32 active inference requests, overload503 and health200.
Shutdown measurements qualify graceful **idle** shutdown, not active-stream
shutdown deadlines or restart/replay integrity. Those existing Steve guarantees
are retained, not requalified exhaustively by this spike.

After graceful shutdown, only `chat.attempt.terminal.v1` exists durably. Responses
and Messages native telemetry has zero correlated durable terminal events.
Each recorded Chat attempt has a unique event/attempt ID; a retried request has
two legitimate attempts. This establishes the current accounting gap; it does
not implement full cross-protocol billing. Any accounting extension needs its
own bounded plan covering success/error/timeout/disconnect, missing/partial
usage, duplicate/replay identity and restart incidents.

Gateway access/content logging is disabled and runtime level is error for this
experiment. Synthetic content/client/provider key canaries were absent from
retained runtime logs. That configuration does not remove the adapter's unsafe
WARN parsing path; production adoption requires redaction/filtering tests across
malformed JSON/SSE/tool content while preserving useful sanitized diagnostics.

## Measured result on Darwin arm64

One local run; the same debug Steve binary is used in both stacks, the gateway
is the official release-candidate binary. Thirty sequential small JSON requests
per path follow the functional checks. These are illustrative warm local
observations, not a capacity test, confidence interval or production SLA. CPU
scheduling, protocol order and allocator history can affect this small sample.

| Measurement | Steve direct | Steve plus gateway |
|---|---:|---:|
| Steve listener-ready startup | 58.11 ms | 57.09 ms |
| Extra gateway TCP-listener startup | none | 22.84 ms |
| Warm Chat JSON first-body-byte median | 1.513 ms | 1.690 ms |
| Sequential fixture JSON throughput | 512.1 requests/s | 458.8 requests/s |
| Post-probe stack RSS | 30416 KiB | 62656 KiB |
| Idle shutdown | 287.55 ms | 341.01 ms (Steve only) |

All six normal protocol/mode cases returned200. SSE first-byte delays were
2.33 ms or less direct and
4.59 ms or less through the gateway,
with a300ms delayed tail: neither buffered the complete stream.

Messages-to-Responses conversion succeeded in JSON and SSE with expected text;
the fixture observed `/v1/responses`, while direct Steve retained `/v1/messages`.
The earlier minimal Responses fixture lacked required usage-detail objects and
failed typed JSON conversion; completing the synthetic schema resolved it. This
is fixture qualification, not evidence that real provider responses fail.

Each path has70 durable Chat terminals,70 unique eventIDs and70 unique attemptIDs,
correlated to69 native Chat requestIDs (the503 case has two attempts). Responses
and Messages each have zero correlated durable terminals. This is an existing
Steve limitation, unaffected by adding a gateway.

Raw measurements and sanitized attempt metadata are in [results.json](results.json).
Its script SHA256 and both binary hashes bind the observations to the executed
probe. No stack adoption or final release merge is approved by these results.
