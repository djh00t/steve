# STV-M2-14A (#576): listener and remote TLS boundary proposal

**State: proposal for review only; no boundary below is accepted yet.** Publication, green CI, or READY status alone is not acceptance. David or Cos reviewing and merging this exact artifact revision accepts the proposal for #106, but does not authorize runtime implementation or deployment. TLS implementation and qualification remain separate work owned by an explicitly named owner.

## Scope and current behavior

This artifact narrows the listener exposure and remote TLS portion of [#106](https://github.com/djh00t/steve/issues/106). It does not implement listeners, TLS, certificate provisioning, proxy termination, management bearer authentication, or inference authentication.

On current main `675533dbe0e2d00af9ec71ca59eb34d9151ba63f`:

- `ServerConfig::default` binds inference to `[::]:11435` and management to `[::]:8790`; both wildcard addresses are remote exposure.
- `server.rs` parses each configured bind as a numeric `SocketAddr`, then binds the inference listener and management listener. There is no TLS setup or remote-address rejection.
- The security specification requires loopback defaults and authenticated TLS for remote deployment ([spec section 14](../specs/2026-09-26-steve-gateway.md#14-security)).

No runtime test or TLS qualification is evidence for this documentation proposal.

## A — loopback defaults and classification

**Proposed boundary:** fresh defaults are `127.0.0.1:11435` for inference and `127.0.0.1:8790` for management. An explicitly configured `127.0.0.1` or `[::1]` address is loopback. Classify exposure with `addr.ip().is_loopback()`; every other address, including IPv4/IPv6 wildcards, is remote.

`server.inference_bind` and `server.management_bind` remain string configuration fields. They must parse as complete numeric `SocketAddr` values. Hostnames, partial values, malformed values, and empty values fail startup before either listener binds. Existing environment overrides remain supported, with `STEVE_INFERENCE_BIND` taking precedence over `STEVE_BIND` for inference and `STEVE_MANAGEMENT_BIND` applying to management. Do not silently replace an invalid or remote value.

Changing the defaults intentionally removes implicit LAN exposure. An old configuration that explicitly names a wildcard is remote under this proposal and fails closed unless the remote boundary below is complete. There is no automatic config rewrite.

## B — proposed remote TLS boundary

**Proposed decision for review:** the only implementable remote mode is `native_tls`: Steve terminates TLS on both configured listeners with one validated server certificate and private key. Require TLS 1.2 or newer. If either listener is remote, both listeners use TLS; no plaintext listener, downgrade, or fallback is permitted. A loopback-only configuration may remain HTTP for local development.

The proposed configuration is strict:

```toml
[server.security]
tls_mode = "disabled" # or "native_tls"
tls_cert_file = "/protected/steve/server.crt"
tls_key_file = "/protected/steve/server.key"
```

`disabled` is the default only for loopback binds and must not be used with a remote address. With `disabled`, both `tls_cert_file` and `tls_key_file` must be absent; a path in either field is a startup error. `native_tls` requires both non-empty paths. `external_proxy` and all other values are errors; no external proxy termination mode is accepted by this proposal. Unknown security keys are errors. TLS library/dependency selection, supported platforms, and executable qualification are intentionally deferred.

Remote TLS authenticates the server to clients; it does not authenticate callers. A non-loopback inference bind remains rejected until [#502](https://github.com/djh00t/steve/issues/502) accepts and implements its caller-authentication boundary. After that work, remote inference requires both caller authentication and this TLS boundary. Remote management also requires the separate management-authentication boundary in [#577](https://github.com/djh00t/steve/issues/577).

### Startup validation and failure handling

Before either listener binds, startup must validate all configured addresses, the remote/loopback relationship, the TLS mode, and (for `native_tls`) the certificate and key:

- Both paths must be present, readable regular files containing valid PEM material.
- The certificate and private key must match. Missing one half, inaccessible or malformed PEM, a mismatch, or syntactically valid but unsupported or encrypted private-key material fails startup.
- The private-key file must be restricted to the Steve daemon user and required privileged operating-system identities. Reject group/world-readable files, files readable by unrelated users, or any access policy Steve cannot verify.
- Error messages may identify the failing configuration key or path, but must never include private-key bytes, certificate contents beyond safe path/config context, tokens, or secret-file contents.

Any validation failure occurs before either socket is opened. There is no warning-and-continue, plaintext fallback, or “skip verification” server mode. Remote clients remain responsible for validating certificate chain and endpoint name; local fixtures may use a certificate that is not publicly trusted.

## C — downgrade and forwarded-header boundary

`Forwarded`, `X-Forwarded-Proto`, and related headers are request input and cannot prove that the transport was encrypted. They must not enable remote trust, switch `tls_mode`, bypass caller or management authentication, or make a plaintext listener acceptable. This proposal defines no trusted-proxy peer identity or protected local transport contract, so proxy-only termination remains deferred.

## Rotation, rollout, and rollback

Certificate and key material are read and validated at startup; rotation is restart-only. Provision the new pair before restarting, start the candidate with both listeners validated, then remove the old pair only after readiness and certificate checks succeed. A failed candidate validation opens no listener socket. Process replacement, uptime, and keeping the old process serving during a restart belong to an explicitly named supervisor or deployment mechanism outside this artifact; the current fixed-port server cannot promise that behavior itself.

Rollback restores the prior binary and its compatible configuration together through that supervisor or deployment mechanism. Restoring an old wildcard value alone under a revision enforcing this boundary remains rejected. A proxy-only deployment has no migration path in this proposal.

## Review matrix

| Given | When | Then |
|---|---|---|
| Fresh configuration or explicit loopback binds | Listener addresses are parsed and classified | Defaults are exactly `127.0.0.1:11435` and `127.0.0.1:8790`; `[::1]` is loopback; malformed/hostname values fail before either bind. |
| Any non-loopback listener | Startup evaluates security mode | `disabled` fails before binding; `native_tls` is required for both listeners; remote inference also waits for accepted #502 caller authentication. |
| `tls_mode = "disabled"` with either certificate/key path present | Startup validates the security table | Startup fails before either listener binds; both paths must be absent in disabled mode. |
| Missing, unreadable, malformed, mismatched, unsupported, encrypted, or insecure certificate/key material | `native_tls` startup validation runs | Startup fails before either socket opens and diagnostics contain no key/certificate contents. |
| A client supplies forwarded or downgrade-related headers | Request reaches a listener | Headers cannot establish TLS, trusted proxy status, or authentication. |
| A valid replacement pair is provisioned | Operator restarts, then checks readiness | The pair is validated before the candidate opens either listener; supervisor/deployment rollback uses the prior binary/config pair if validation fails. |

## Handoff to #143

[#143](https://github.com/djh00t/steve/issues/143) consumes only the accepted listener/TLS boundaries. It must be re-sized against the accepted revisions of A and B and remain blocked until a TLS implementation and qualification owner is named. Its qualification must cover real daemon startup and both listeners, certificate/key failure and redaction, no plaintext downgrade, spoofed forwarded headers, restart-only rotation, rollback, and the #502 inference-authentication gate. This proposal supplies no passing runtime evidence and does not make #143 ready.

## Review record

- Source review: `src/config.rs`, `src/server.rs`, current proposal [`stv-m2-14.md`](stv-m2-14.md), and spec section 14.
- Issue brief: [#576](https://github.com/djh00t/steve/issues/576), read against main `675533dbe0e2d00af9ec71ca59eb34d9151ba63f`.
- Repository validation: `make check` passed on this candidate (`cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo check --all-targets --all-features`); this is gate evidence only, not security-policy acceptance.
- Acceptance: `- [ ] David or Cos accepts this exact revision` (authority/date/revision: ______).
