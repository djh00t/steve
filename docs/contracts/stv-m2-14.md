# STV-M2-14 #106: management and listener security proposal

**State: proposal for review; no boundary below is accepted.** READY status, review of this file, or merge of an unrelated PR accepts none of these choices. David or Cos must explicitly accept each boundary. #106 closes only after A, B, and C are accepted. Downstream implementation is blocked until then and must be re-sized against the accepted revision.

## Evidence and current mismatch

`ServerConfig::default` currently binds inference to `[::]:11435` and management to `[::]:8790`. `inference_router` and `management_router` add no authentication. The management router currently exposes live/ready health, version, status, drain, and provider health. The spec requires loopback defaults and authenticated TLS for remote deployment. These current defaults therefore expose both listeners beyond loopback and management has no authentication. See `src/config.rs`, `src/server.rs`, and [spec section 14](../specs/2026-09-26-steve-gateway.md#14-security).

## A — listener defaults and exposure

**Proposed decision:** default inference and management binds to `127.0.0.1:11435` and `127.0.0.1:8790`. Explicit `127.0.0.1` and `[::1]` binds count as loopback. Use `addr.ip().is_loopback()` to classify exposure. Any non-loopback address is remote and is permitted only under boundary B; wildcard addresses count as remote. Do not silently fall back to a different address.

**Proposed config:** existing `server.inference_bind` and `server.management_bind` remain strings, with the loopback defaults above. Both must parse as numeric IP socket addresses (`SocketAddr`); hostnames, partial values, and malformed values fail startup before either listener binds. Existing bind environment overrides remain supported, with the existing listener-specific override taking precedence over `STEVE_BIND` for inference. No null values are valid for either field.

**Compatibility/startup:** changing the defaults intentionally removes implicit LAN exposure. Deployments that relied on `[::]` must opt into remote exposure through B. No automatic migration rewrites old config: an explicit wildcard in old config is treated as remote and fails closed unless B is complete. Rollback means restoring the prior application revision and its prior config deliberately; restoring the old wildcard alone under the new revision remains rejected.

**Acceptance — David or Cos:** `- [ ] Accept A` (authority/name/date/revision: ______)

## B — remote TLS trust and enforcement

**Proposed decision:** implementable remote mode is `native_tls`: Steve terminates TLS on both configured listeners using a server certificate and private key. Require TLS 1.2 or newer and certificate/key validation before binding. If either listener is non-loopback, both listeners use TLS; there is no plaintext listener, downgrade, or fallback. This is a server-authenticated TLS boundary; management bearer authentication in C remains required separately. A non-loopback inference bind must still fail startup until #502’s authenticated inference boundary is accepted and implemented; TLS server authentication alone does not authenticate inference callers. This gate is a daemon capability, not a config switch, proxy header, or caller assertion. After #502 is implemented, remote inference requires both its accepted caller authentication and this TLS boundary. Management remote exposure may proceed under B and C independently.

The alternative `external_proxy` (TLS terminates outside Steve) is named but deferred. Forwarded headers such as `Forwarded` and `X-Forwarded-Proto` are attacker-controlled and cannot prove TLS. Current source has no trusted-proxy peer identity or protected local transport contract. Reconsider this option only with a concrete daemon-enforced trust mechanism and separate acceptance. No TLS library or implementation is selected here; TLS support must be qualified separately before implementation is sized.

**Proposed config:** add strict `[server.security]` keys:

- `tls_mode`: string enum `disabled` (default) or `native_tls`; `external_proxy` and all other values are errors.
- `tls_cert_file`, `tls_key_file`: optional string paths, both absent by default (TOML has no null value). With `disabled`, both must be absent and all binds must be loopback. With `native_tls`, both must be non-empty paths to readable, valid PEM certificate and matching private key files; every permitted listener uses TLS. The certificate path must be a readable regular file. The private-key path must be a regular file restricted to the Steve daemon user and required privileged operating-system identities; reject group/world-readable files, files readable by unrelated users, or files whose access policy Steve cannot verify. Validate these requirements before binding and fail closed if access cannot be verified. Never log or include private-key material in errors or diagnostics. One missing path, inaccessible/malformed PEM, mismatch, insecure or unverifiable key-file access, or an unsupported key fails startup before binding.
- Unknown keys in `[server.security]` are errors. Empty strings are errors, not null. Existing unrelated config parsing is outside this proposal.

A certificate that does not chain to a client trust store may still be used for local fixtures; remote clients must validate its chain and endpoint name. Do not add a “skip verification” client behavior to Steve's server policy.

**Compatibility/startup/rollback:** old config has no security table and takes `tls_mode = "disabled"`; loopback binds can start once C’s token file is provisioned. Remote management binds fail until both TLS files and `native_tls` are configured. Remote inference also remains rejected until #502’s accepted authentication boundary is implemented, even with valid TLS config. TLS files are read and validated at startup; rotation requires a restart in this proposal. Rollback requires restoring the previous binary and its config together. A proxy-only deployment has no accepted migration path in this proposal.

**Acceptance — David or Cos:** `- [ ] Accept B` (authority/name/date/revision: ______)

## C — management authentication

**Proposed decision:** require one independent management-admin bearer token on every management request, including live/ready health, version, status, drain, provider health, all methods, and unknown paths before route/fallback resolution. That single credential has the fixed `management.admin` scope; there is no role lookup or per-user authorization policy in this proposal. No route is exempt. This does not create Organisation/User/Client schemas and does not depend on #99 or bootstrap child #501. Inference routes and inference identity stay owned by #502. The token is not an inference credential.

**Proposed config/credential source:** `[server.security].management_token_file` is a required non-empty string path, with optional environment override `STEVE_MANAGEMENT_TOKEN_FILE`. An explicitly present empty override is an error; a non-empty environment value overrides the TOML path. The file is read once during startup as one unpadded base64url token encoding exactly 32 random bytes (43 ASCII characters), optionally followed by one LF; reject other bytes, extra lines, or shorter values. The path must identify a regular file restricted to the Steve daemon user and required privileged operating-system identities. Reject group/world-readable files, files readable by unrelated users, or files whose access policy Steve cannot verify; platform-specific permission checks must be qualified before that platform is supported. The operator must provision it through the deployment's protected secret mechanism; Steve never creates or prints a bootstrap secret. Reload/rotation is restart-only. The sample below is fixture-only and must never be deployed.

Clients send `Authorization: Bearer <token>`. Missing, malformed, or incorrect credentials receive the same `401` response with `WWW-Authenticate: Bearer` and `Cache-Control: no-store`; do not accept query, cookie, forwarded, or alternate header credentials. Valid credentials proceed normally, including to a `404` for an unknown path. Compare decoded token bytes in constant time. Never log request authorization values, token contents, or secret-file contents. TLS is mandatory for remote use; loopback HTTP is an explicit local-development exception and is not a claim of full [RFC 6750](https://www.rfc-editor.org/rfc/rfc6750) conformity. This exact auth scheme and lifecycle remain proposed pending acceptance.

**Startup errors:** missing path, inaccessible/non-regular/insecure file, unverifiable access policy, invalid encoding/length, or an empty environment override fails before either listener binds. No missing-secret warning-and-continue mode or unauthenticated bootstrap exists. Error messages may name the config key or file path but must not include file contents.

**Compatibility/startup/rollback:** all current management callers become authenticated; health probes must send the token. Existing configs fail startup until the secret path is provisioned. During rollout, provision the secret before restarting the new binary; rollback requires restoring the prior binary and old config/health-probe behavior together. Token rotation is provision-new-file-then-restart; remove the old secret only after the restart is confirmed.

**Acceptance — David or Cos:** `- [ ] Accept C` (authority/name/date/revision: ______)

## Dummy fixture config

This shows proposed field names and rules only. The token is deliberately dummy data; replace the path/value in any real fixture with a generated secret.

```toml
[server]
inference_bind = "127.0.0.1:11435"
management_bind = "[::1]:8790"

[server.security]
tls_mode = "disabled"
management_token_file = "/tmp/steve-test-management-token"
```

Fixture-only dummy file content (syntactically valid; never use as a real secret): `AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA`

## Future qualification matrix — proposed names, not test evidence

Consumer boundary: #109 must be re-sized after acceptance because its current brief says non-health routes/admin scope, while this proposal protects health too and defines one fixed management-admin credential. #143 consumes only the listener/TLS boundaries; it must be re-sized against accepted A/B and remain blocked until a TLS implementation/qualification owner is explicitly named and accepts that work. Neither issue is runtime-ready from this proposal alone. No runtime test has been run or is claimed for this documentation proposal. After all three boundaries are accepted, the implementation owner should re-size and add real-daemon E2E cases; suggested names and command are unverified and must be confirmed on that candidate branch.

| Proposed case | Required result |
|---|---|
| `security_fresh_defaults_bind_loopback_v4_and_v6` | Fresh defaults bind only loopback; explicit IPv6 loopback remains allowed. |
| `security_management_rejects_missing_and_invalid_bearer` | Missing, malformed, and invalid token all return the same 401 and do not invoke a handler. |
| `security_management_authenticates_health_and_drain` | Authenticated health succeeds; authenticated drain reaches its handler; unauthenticated requests to both are rejected. |
| `security_rejects_malformed_or_partial_tls_before_bind` | Missing half of cert/key pair, malformed PEM, and mismatched key fail startup before sockets open. |
| `security_refuses_nonloopback_plaintext` | Any non-loopback listener with `tls_mode=disabled` fails before binding; no plaintext fallback occurs. |
| `security_ignores_spoofed_forwarding_headers` | Forwarded/X-Forwarded-* headers cannot enable trust or bypass TLS/auth requirements. |
| `security_authenticates_unknown_paths_before_fallback` | No token yields 401 even for an unknown path; valid token gets ordinary 404. |
| `security_never_leaks_management_secret` | Responses, startup errors, and request logs contain no token or secret-file contents. |

Proposed command: `cargo test security_ -- --nocapture` (future implementation qualification only; not run and not passing evidence).

## Unresolved evidence boundary

Source does not establish a TLS server implementation, deployment certificate lifecycle, cross-platform secret-file permission validation, or an enforceable proxy peer identity. A TLS implementation/qualification owner must be named and accept that work before #143 can be considered ready. This proposal therefore chooses native TLS and restart-only certificate/token reload, requires regular secret files whose permissions are restricted to the daemon and required privileged identities; platform-specific enforcement must be qualified, and defers external proxy termination. Accepting those choices is an owner decision; implementation-specific qualification remains required.
