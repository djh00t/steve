# STV-M2-14B #577: management authentication boundary proposal

**Status: proposal only.** This document records a security boundary for review; it changes no runtime behavior and does not accept a policy, implementation, or deployment. David or Cos must review and merge this exact revision before [#106](https://github.com/djh00t/steve/issues/106) can compose it or [#109](https://github.com/djh00t/steve/issues/109) can be re-sized; approval of unmerged content is not sufficient. Inference authentication remains owned by [#502](https://github.com/djh00t/steve/issues/502).

## Current route inventory

At source revision `675533dbe0e2d00af9ec71ca59eb34d9151ba63f`, `management_router` exposes:

| Method | Path | Current handler |
| --- | --- | --- |
| `GET` | `/health/live` | liveness |
| `GET` | `/health/ready` | readiness |
| `GET` | `/api/v1/system/version` | version |
| `GET` | `/api/v1/system/status` | status |
| `POST` | `/api/v1/system/drain` | drain |
| `GET` | `/api/v1/providers/health` | provider health |

The current router has admission and request tracking middleware but no authentication. The proposed authentication middleware runs on every request reaching the management listener, before `admit_management`, route, or fallback resolution. Authentication therefore has precedence over admission saturation: unauthenticated requests receive the uniform auth failure even when the management budget is full. It covers the listed routes, unsupported methods, and unknown paths. No health, status, drain, provider-health, or fallback exemption exists.

This boundary applies only to management. Inference routes and resolved inference identity remain separate [#502](https://github.com/djh00t/steve/issues/502) decisions. Specification §14 requires authenticated TLS for remote deployment; [#576](https://github.com/djh00t/steve/issues/576) is still proposal-only and proposes the listener/TLS details. Bearer authentication does not replace that TLS requirement. This proposal does not create `Organisation`, `User`, or `Client` schemas and does not depend on [#99](https://github.com/djh00t/steve/issues/99), [#500](https://github.com/djh00t/steve/issues/500), or [#501](https://github.com/djh00t/steve/issues/501).

## Proposed credential boundary

Use one independent management-admin bearer token with the fixed scope `management.admin`. There is no principal lookup, role table, per-route policy, or inference credential reuse in this proposal. The token is an operator credential for the management listener.

The daemon reads the token from a protected regular file named by `[server.security].management_token_file`. `STEVE_MANAGEMENT_TOKEN_FILE`, when present and non-empty, overrides that path; an explicitly present empty value is an error. The file contains exactly one unpadded base64url encoding of 32 random bytes (43 ASCII characters), optionally followed by one LF. Reject empty content, extra lines, other bytes, invalid encoding, and any decoded length other than 32 bytes. Steve never creates, prints, or bootstraps this secret.

The path must be readable at startup and its access policy must be verifiably restricted to the Steve daemon user and required privileged operating-system identities. Reject a directory, symlink policy that cannot be verified, group/world-readable content, unrelated-user readability, or any platform where the required restriction cannot be qualified. Platform-specific permission checks remain implementation qualification work.

Clients send `Authorization: Bearer <token>`. Do not accept query parameters, cookies, forwarded headers, alternate authorization headers, or token values in request bodies. Decode the presented token and compare equal-length decoded bytes in constant time. Never cache authorization decisions across requests.

## Request outcomes and disclosure rules

Authentication is checked before route or fallback resolution:

- Missing, malformed, wrong-scheme, wrong-length, invalidly encoded, or incorrect credentials all return the same `401 Unauthorized` response with `WWW-Authenticate: Bearer` and `Cache-Control: no-store`.
- The response body, status, and headers for those failures do not reveal whether a route exists or whether a token was well-formed.
- A valid token proceeds to ordinary route behavior. A valid request to an unknown path receives the normal `404 Not Found`; a valid request using an unsupported method receives the router's ordinary method outcome.
- Authentication failures do not invoke `admit_management`, a route handler, drain action, provider probe, fallback handler, or request-side telemetry. Any safe redacted operational diagnostic must be a process-level record outside request handling and must not include tokens, authorization values, secret contents, or unbounded attacker-controlled path/value labels.

Do not log authorization headers, presented tokens, token-file contents, or decoded secret bytes. Startup errors may identify the configuration key and sanitized file path, but never secret contents or derived token material. `Cache-Control: no-store` applies to HTTP authentication-failure responses; diagnostics are redacted logs, not cacheable HTTP responses.

## Startup, lifecycle, and rollback

The token path is required for this proposal. Missing, unreadable, non-regular, insecure, unverifiable, malformed, or incorrectly sized material, including an empty environment override, fails startup before either listener binds. There is no warning-and-continue mode, unauthenticated bootstrap route, default token, or generated fallback.

The token is loaded once during startup as one decoded 32-byte value held in bounded process memory for constant-time request checks. It is not persisted, emitted to logs, or included in responses. This proposal does not claim memory zeroization on drop, protection from OS swap, or that all transient parser copies are eliminated; implementation qualification must document any stronger guarantee it can prove. Rotation is restart-only and keeps old and new credentials independently recoverable until verification:

1. Provision a new protected token file at a distinct path and retain the current path/configuration.
2. Restart the daemon with the new path/configuration.
3. Confirm authenticated health/status access and ordinary rejection of the old token.
4. Remove the old secret only after the new process is confirmed healthy.

Credential rollback restores the prior token path/configuration and restarts the current compatible binary; if verification of the new credential fails, restore that prior path/configuration and restart before removing either secret. Release rollback is separate: restore the prior binary together with its compatible configuration and health-probe credentials. Restoring only an old token or only an old binary is not a supported release rollback. A failed pre-bind validation leaves both listeners unbound and does not expose secret material.

## Review matrix

| Given | When | Then |
| --- | --- | --- |
| Any current management route, unsupported method, or unknown path | Credentials are absent, malformed, or invalid | The same `401` bearer challenge is returned before route/fallback resolution; no handler or side effect runs. |
| A valid management token | A protected route is called | Normal route behavior runs, including health/status/drain/provider health. |
| A valid management token | An unknown path is called | The ordinary router `404` is returned, proving authentication preceded fallback resolution. |
| Missing, unreadable, insecure, malformed, or unverifiable token material | Startup is attempted | The daemon fails before either listener binds and does not disclose secret contents. |
| A new token file is provisioned at a distinct path | The daemon restarts and verification succeeds | Only the new token authenticates; the old credential remains available for credential rollback until removal. |
| Management admission is saturated | An unauthenticated request arrives | Authentication runs first and returns the same `401`; no admission permit or management handler is consumed. |

## Handoff to #109

After this exact artifact revision is reviewed and merged, [#109](https://github.com/djh00t/steve/issues/109) must be re-sized against the merged revision. Its current brief says “all non-health management routes”; this proposal requires the same admin bearer check for health, status, drain, provider health, unsupported methods, and unknown paths. The implementation consumer owns the middleware, route-wide negative coverage, and the real-daemon scenario `management_exposure_security`; it must preserve the pre-bind failure and redaction guarantees here. Add the future saturation assertion `management_auth_precedes_admission_under_saturation` to that scenario, covering an invalid request receiving `401` while admission is full; this is a reserved test requirement, not executed evidence. #109 remains blocked until its listed identity, listener/TLS, test, and contract prerequisites are accepted.

## Evidence boundary

This proposal was reviewed against `src/server.rs:management_router`, `src/config.rs:ServerConfig`, [STV-M2-14](stv-m2-14.md), [specification §14](../specs/2026-09-26-steve-gateway.md#14-security), issue [#577](https://github.com/djh00t/steve/issues/577), and the current [#109](https://github.com/djh00t/steve/issues/109) brief at source revision `675533dbe0e2d00af9ec71ca59eb34d9151ba63f`.

No runtime management-auth behavior is implemented or claimed by this document. Repository checks are delivery evidence only; they do not constitute acceptance of this security proposal. Runtime qualification, TLS implementation, secret provisioning, deployment testing, and policy acceptance remain downstream work. The exact revision must be reviewed and merged before #106 composition or #109 re-sizing; unmerged approval is not acceptance.

Repository gate rerun on 2026-09-28: `make check` passed. Exact commands and results were `cargo fmt --all -- --check` (pass), `cargo clippy --all-targets --all-features -- -D warnings` (pass), and `cargo check --all-targets --all-features` (pass; dev profile finished successfully). Scoped security re-review made no runtime changes; this same gate was rerun after the wording correction.

**Acceptance gate — David or Cos must review and merge this exact #577 artifact revision:** `- [ ] Accepted` (authority/name/date/revision: ____________________)
