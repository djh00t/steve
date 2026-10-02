# Steve 0.2.1 cache-directive release candidate

The 0.2.0 native candidate discarded upstream response cache headers while constructing JSON/SSE/error replies. This patch preserves repeated Cache-Control plus Pragma, Expires, Vary, Date and Age. It excludes response cookies, credential/custom headers, hop-by-hop headers and any otherwise allowed header named by Connection. Metadata is local to each upstream request; transport failures without a response have none. Routing, credential substitution, provider billing and streaming cancellation remain in the existing flow.

Version 0.2.1 is a patch increment from the approved 0.2.0 candidate for `fix(proxy): preserve provider cache directives`; it does not claim 0.2.0 was publicly published. Cargo version matches this candidate. Caching is disabled by default and no vSR runtime, model service, dependency or installed-app change is included. The proposed vSR admission patch is [a separate design](cache-admission-design.md), not a shipped safeguard.

## Fixture demo

Use an isolated checkout of this candidate on a supported native target, with Rust/Cargo and Python3 available. No SDK install or real provider credentials are needed. The script launches only candidate-owned loopback fixtures and temp Steve state; its child environment contains fake credentials only.

```sh
cargo build --locked
python3 -B experiments/agentgateway/cache_headers_demo.py \
  --steve "$PWD/target/debug/steve" \
  --output /tmp/steve-cache-header-results.json
```

Expected: 12 cases, zero failures, error null, cleanup true. Every JSON/SSE success/error response preserves the repeated `private` and `No-Store` values, Pragma and Vary; sensitive fixture headers are excluded. Native provider auth substitution remains correct. Chat JSON503 retains its existing two attempts; the other tested paths make one. No response cache is enabled. Focused Rust regression checks additionally cover Connection-nominated exclusions and request-local metadata. The script removes its temporary config, database/accounting state and stops its own children; only the requested sanitized result file remains. Delete that file when no longer needed.

## Gateway and official cache boundary

The pinned gateway-alone fixture preserved provider no-store. The previously executed official vSR v0.4.0 ARM64 integration demonstrated an exact JSON hit but cached a provider no-store response. Therefore preserving the Steve header is necessary and does not make that vSR cache safe. The cache-admission design requires pre-storage enforcement and fresh isolated cache state; it cannot be repaired by stripping already-stored hits.

Authenticated tenant isolation, actual cache-hit ledger semantics, streaming/tool-history cache admission, real-provider savings and compressor quality are not qualified by this release. Unknown usage remains unknown; repeated cached wire usage must never be presented as new provider billing. No real keys or paid requests are part of acceptance. Human release merge and installed-app update require separate approval.
