# STV-PROV-38: upstream TLS backend and trust contract (proposal)

**Decision requested:** pin reqwest to `=0.12.28`, enable Rustls with bundled WebPKI roots, and allow one optional additive PEM CA bundle. This is a planning proposal reviewed against `0f5f72506e28fb172c5339dddc9733482a5f41e5`; production `main` remains `6d635f546b66514ed96393803806e12015add4dc`. No runtime or dependency change is included here. Rustls plus bundled roots avoids platform-native TLS differences and keeps public trust roots explicit and portable across developer, test, and server environments.

## Contract

- Use `reqwest = { version = "=0.12.28", default-features = false, features = ["json", "stream", "rustls-tls-webpki-roots"] }`. Keep certificate-chain and hostname validation enabled. Never use either dangerous certificate/hostname bypass.
- Add `[server].upstream_ca_bundle: Option<PathBuf>` (default absent). Absent trusts bundled WebPKI roots. When set, read a PEM certificate bundle at startup and add each certificate to WebPKI roots. Reject unreadable, empty, or malformed bundles before listeners become ready; report the path and error kind, never certificate contents. Filesystem-relative paths use the process working directory, like existing relative paths passed to filesystem APIs. Changes require restart. This option accepts public CA certificates only, not keys; it does not replace WebPKI roots.
- Accept absolute `https` provider URLs. Accept `http` only when the URL host parses directly as an IP address and that IP is loopback (`127.0.0.0/8` or `::1`). Reject all other cleartext, including `http://localhost`, without DNS resolution. Keep the existing separate OpenAI `/v1` URL normalization and Anthropic `/v1` normalization. Certificate validation stays enabled for every HTTPS request.
- Apply the same additive CA roots and redirect `Policy::none()` to OpenAI, Anthropic, and provider-health requests. Preserve Anthropic's `pool_max_idle_per_host(0)` streaming-cancellation behavior. Inference keeps its current non-success handling for 3xx; health may continue to call 3xx reachable, which does not mean inference succeeded.
- TLS transport only is in scope. Do not add provider credentials, account routing, mTLS client keys, live reload, global environment changes, or a TLS bypass. Existing fixture HTTP remains supported only on numeric loopback.

## Source trace and implementation seam

At `0f5f72506e28fb172c5339dddc9733482a5f41e5`, `Cargo.toml` disables reqwest defaults and enables only JSON/stream. `OpenAiUpstream::new` and `AnthropicUpstream::new` each build a redirect-disabled client; Anthropic also disables idle pooling. Their `normalize_base_url` functions separately require `http` (`src/proxy/openai_upstream.rs`, `src/proxy/anthropic_upstream.rs`). `server::run` constructs those adapters and `ProviderProbeState::new` separately constructs a third client (`src/server.rs`). Config is loaded in `Config::load` and passed to `server::run`; `Config.source` is only used for display. Existing relative journal paths go directly to filesystem APIs, so relative CA paths are cwd-relative, not config-file-relative.

Smallest shared seam: add one crate-private provider URL validation helper in `src/proxy/mod.rs` and call it from both normalizers; keep each provider's existing path normalization and error type. Parse the CA bundle once at startup and build two redirect-disabled reqwest clients from the same trust roots: a normal client shared by OpenAI and provider health, and an Anthropic client retaining its no-idle-pool setting. Add crate-private constructors that accept these clients; preserve current `new` constructors for existing unit callers by having them build the default equivalent. Do not add an invented CA abstraction or public configuration API beyond the `ServerConfig` field.

## Dependencies and fixture encoding

The following Dependency Advisor reports were observed on 2026-09-27. They are dated evidence, not permanent approval; refresh them before implementation if they have expired.

<details>
<summary>reqwest Dependency Advisor report</summary>

```json
{
  "advisory_sources": [],
  "candidates_considered": 126,
  "ecosystem": "rust",
  "minimum_release_age_hours": 336,
  "package": "reqwest",
  "policy": "standard",
  "provenance": {
    "cache_status": "hit",
    "expires_at": "2026-09-28T08:58:16.836796Z",
    "fetched_at": "2026-09-27T08:58:16.836796Z",
    "metadata_url": "https://crates.io/api/v1/crates/reqwest",
    "source": "crates.io"
  },
  "reason": "Selected newest candidate that satisfies policy checks.",
  "recommended_version": "0.12.28",
  "source": "crates.io",
  "status": "recommended"
}
```

</details>

<details>
<summary>tokio-rustls Dependency Advisor report</summary>

```json
{
  "advisory_sources": [],
  "candidates_considered": 79,
  "ecosystem": "rust",
  "minimum_release_age_hours": 336,
  "package": "tokio-rustls",
  "policy": "standard",
  "provenance": {
    "cache_status": "miss",
    "expires_at": "2026-09-28T09:07:43.749097Z",
    "fetched_at": "2026-09-27T09:07:43.749097Z",
    "metadata_url": "https://crates.io/api/v1/crates/tokio-rustls",
    "source": "crates.io"
  },
  "reason": "Selected newest candidate that satisfies policy checks.",
  "recommended_version": "0.26.5",
  "source": "crates.io",
  "status": "recommended"
}
```

</details>

Use the recommended exact versions in the handoffs: reqwest `=0.12.28` and dev-only `tokio-rustls = { version = "=0.26.5", default-features = false, features = ["ring", "tls12"] }`. No rcgen, pemfile, or new test framework.

The formats serve different APIs: `upstream_ca_bundle` is PEM because the reqwest contract reads PEM certificate bundles. The local TLS server uses checked-in DER leaf-certificate and private-key files with tokio-rustls. Keep a PEM CA bundle for reqwest trust; if the TLS fixture needs the CA in Rustls form, provide the CA certificate as DER too. Never call DER server certificates “PEM CAs” or store private keys in the CA bundle. The fixture leaf must contain an IP SAN for `127.0.0.1`; its key is test-only.

Versioned references: [reqwest 0.12.28 features](https://docs.rs/reqwest/0.12.28/reqwest/), [ClientBuilder TLS options](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html), [reqwest v0.12.28 Cargo.toml](https://raw.githubusercontent.com/seanmonstar/reqwest/v0.12.28/Cargo.toml), [tokio-rustls 0.26.5](https://docs.rs/tokio-rustls/0.26.5/tokio_rustls/), [tokio-rustls v0.26.5 Cargo.toml](https://raw.githubusercontent.com/rustls/tokio-rustls/v/0.26.5/Cargo.toml), and [Axum 0.8.9 Listener](https://github.com/tokio-rs/axum/blob/axum-v0.8.9/axum/src/serve/listener.rs).

## Sequenced handoffs

Issue bodies are the dispatch briefs; accepted producer revisions and actual fixture APIs must be present before any leaf becomes READY. #497 follows both positive protocol proofs, and #498 follows #497 because they edit the same test file.

Each implementation handoff is intended to fit 5–10 active minutes. All handoffs require accepted #491 and met prerequisites #72 and #73. Live issue state checked on 2026-09-27 confirms #72 and #73 are closed. Commands are future acceptance gates and become runnable as the named tests are added.

| Handoff | Additional dependencies (all require accepted #491 and met #72/#73) | Owns | Exact acceptance command |
|---|---|---|---|
| `tls-fixture` (#133), 8–10 min | none | `Cargo.toml`, new `tests/support/upstream_tls.rs`, fixed files in `tests/fixtures/tls/`, new `tests/e2e_tls_fixture.rs` | `cargo test --all-features --test e2e_tls_fixture stv_prov_38_fixture_acceptance -- --exact` |
| [`provider-url-policy` (#495)](https://github.com/djh00t/steve/issues/495), 5–7 min | none | `src/proxy/mod.rs`, both upstream modules | `cargo test --all-features proxy::tests::http_requires_numeric_loopback -- --exact` |
| [`shared-upstream-tls` (#496)](https://github.com/djh00t/steve/issues/496), 8–10 min | #133, provider URL policy | `src/config.rs`, `src/server.rs`, both upstream modules, `tests/support/process.rs`, new `tests/e2e_upstream_tls.rs`, `config.example.toml`, affected README config snippets | `cargo test --all-features --test e2e_upstream_tls stv_prov_38_shared_trust_acceptance -- --exact` |
| `openai-upstream-https` (#134), 5–8 min | #133, URL policy, shared TLS | `tests/e2e.rs` | `cargo test --all-features --test e2e stv_prov_32_acceptance -- --exact` |
| `anthropic-upstream-https` (#135), 5–8 min | #134 and its prerequisites | `tests/e2e_messages.rs` | `cargo test --all-features --test e2e_messages stv_prov_36_acceptance -- --exact` |
| [`tls-certificate-rejection` (#497)](https://github.com/djh00t/steve/issues/497), 8–10 min | #496, #134, #135; negative cert fixtures from #133 | focused tests in `tests/e2e_upstream_tls.rs` | `cargo test --all-features --test e2e_upstream_tls stv_prov_38_certificate_rejection -- --exact` |
| [`tls-redirect-rejection` (#498)](https://github.com/djh00t/steve/issues/498), 5–8 min | #496, #134, #135, #497 (single writer) | `tests/e2e_upstream_tls.rs` | `cargo test --all-features --test e2e_upstream_tls stv_prov_38_redirect_rejection -- --exact` |

**#133:** owns the exact manifest entries: `reqwest = { version = "=0.12.28", default-features = false, features = ["json", "stream", "rustls-tls-webpki-roots"] }` and dev-only `tokio-rustls = { version = "=0.26.5", default-features = false, features = ["ring", "tls12"] }`. Build one small Axum-compatible TLS listener with at most four concurrent handshakes, a five-second handshake timeout, orderly shutdown, and task cleanup. Its fixture acceptance uses reqwest trusting the checked-in CA and captures one request. Check in fixtures needed by later leaves with isolated failures: the trusted-CA leaf has SAN `IP:127.0.0.1` and validity `2020-01-01` through `2035-01-01`; the unknown-CA leaf has the same correct SAN and validity dates but chains only to an untrusted CA; the wrong-IP-SAN leaf chains to the trusted CA, is valid `2020-01-01` through `2035-01-01`, and has a non-loopback IP SAN; the expired leaf chains to the trusted CA, has SAN `IP:127.0.0.1`, and is valid only `2020-01-01` through `2021-01-01`. Server leaf/key are DER for tokio-rustls; reqwest's trusted CA bundle is PEM. Keep public CA PEM separate from server private key. Do not add rcgen, pemfile, or a test framework.

**`provider-url-policy`:** add one crate-private validator in `src/proxy/mod.rs`; call it from both existing URL normalizers, preserving protocol-specific path normalization and error types. Accept absolute HTTPS. Accept HTTP only for host strings that parse directly to loopback `IpAddr` (`127.0.0.0/8` or `::1`); reject non-loopback IPs and every hostname, including `localhost`, without DNS lookup. The test covers accepted HTTPS, IPv4/IPv6 loopback, `localhost`, and non-loopback IP for both protocol normalizers.

**`shared-upstream-tls`:** depends on `provider-url-policy` so its real-process HTTPS acceptance URL passes the shared validator. Add `ServerConfig.upstream_ca_bundle: Option<PathBuf>` defaulting to `None`; relative paths use process cwd. In startup, read and parse the complete PEM CA bundle once; fail before readiness on unreadable, empty, or malformed input and never log bytes. Build two redirect-disabled clients from the same WebPKI-plus-CA roots: one shared by OpenAI and `ProviderProbeState`, and one for Anthropic retaining `.pool_max_idle_per_host(0)`. Add crate-private constructors `OpenAiUpstream::with_client(base_url: impl Into<String>, timeout: Duration, http: reqwest::Client) -> Result<Self, SteveError>` and `AnthropicUpstream::with_client(base_url: impl Into<String>, timeout: Duration, http: reqwest::Client) -> Result<Self, AnthropicError>`. Keep current `new` entry points delegating to equivalent default clients because current unit callers use them. Extend the process harness to write the CA path and add its real-process test in `tests/e2e_upstream_tls.rs`; verify trusted HTTPS health succeeds, and test unreadable, empty, and malformed CA bundles separately, each making startup fail before readiness. #134 and #135 own the protocol request round trips. No ineffective config-only change is exposed.

**#134 and #135:** each adds its named real-process protocol round trip through the TLS fixture and verifies the captured request; each also checks numeric-loopback HTTP compatibility using the existing fixture. They do not edit the manifest or shared helpers. #135 follows #134 by dependency order.

**`tls-certificate-rejection`:** use the isolated fixtures supplied by #133: unknown issuer with correct SAN/current validity; trusted issuer with wrong IP SAN/current validity; trusted issuer with correct IP SAN/expired validity. Through the configured OpenAI and Anthropic HTTPS request paths, assert each fails for its single intended validation fault. The expired-leaf test checks UTC is within `2026–2034`; outside that window it fails as fixture setup and cannot report a pass.

**`tls-redirect-rejection`:** return a redirect from the TLS upstream and assert OpenAI, Anthropic, and provider health do not follow it. Provider health may retain its existing “3xx reachable” status, but the redirect target must receive no request. URL rejection for non-loopback HTTP is covered by `provider-url-policy`.

## Prerequisites and future gates (not implemented)

- #133 must prove the reqwest-enabled local TLS fixture; the provider E2E leaves use it rather than plain HTTP alone.
- The shared seam must fail startup for invalid CA input and apply the same trust roots to OpenAI, Anthropic, and health. Its acceptance is a real process test, not a duplicate parser-only test.
- #134 and #135 prove trusted HTTPS and numeric-loopback HTTP compatibility. The named certificate-rejection and redirect-rejection handoffs are required before calling TLS qualification complete.
- Tests use fixed local fixtures and no live provider, account, key, or credential. Time-sensitive expired-certificate checks require UTC `2026–2034` and fail as fixture setup outside that interval.
- Provider/account credential binding remains outside this contract and follows the accepted account schema (#100). Gateway listener TLS is a separate boundary and remains unqualified here.

#491 is **READY for exact-artifact review/acceptance only**. All commands above are future acceptance gates; none has been run for this proposal.
