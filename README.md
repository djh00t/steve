# Steve

Steve is an open-source, multi-user LLM gateway and control plane for developers, homes, and small/medium businesses.

Applications talk to one local or remote endpoint. Steve centralises provider credentials, routing, account selection, session tracking, costs, latency, errors, history, and observability.

## Deployment modes

- **Developer local** — native Rust daemon or local OCI container.
- **Home gateway** — shared gateway for multiple people, devices, accounts, and local/cloud models.
- **SMB gateway** — multi-user service with PostgreSQL, S3-compatible storage, SSO/RBAC, and external observability.

The same Rust daemon must support all three.

The optional macOS Swift menu-bar app can:
- discover an existing Steve daemon;
- install Steve natively;
- run Steve via Apple Container/Docker;
- connect to a remote Steve gateway;
- show provider/account usage and health;
- make common routing-profile changes.

## Architecture principles

- Rust daemon is the product; UIs are clients of its management API.
- Native and OCI-container deployments are first-class.
- OpenAI Responses/Chat and Anthropic Messages are first-class ingress protocols.
- Providers and provider accounts are separate entities.
- Multiple accounts per provider are supported with ownership, pools, affinity, quotas, health, and failover.
- Every request is user-, client-, session-, request-, and attempt-aware.
- Canonical model/account pricing is normalised to USD while preserving source currency and FX provenance.
- Local currency can be selected for display; USD price and conversion rate remain visible.
- Latency and errors are measured at every proxy stage and every provider attempt and can influence routing.
- Switchyard may be embedded behind Steve's routing abstraction; Steve remains independent of it.
- SQLite is the zero-admin default; PostgreSQL is the primary larger-install database.
- Local filesystem or S3-compatible object storage holds chat histories and large/raw payloads.
- OpenTelemetry is the standard telemetry export path.
- TUI eventually exposes the full configuration/reporting surface.

## Project documents

- [Architecture and product specification](docs/specs/2026-09-26-steve-gateway.md)
- [MVP plan and backlog](docs/plans/2026-09-26-steve-mvp.md)

## Planned CLI

```text
steve serve
steve tui
steve doctor
steve config
steve providers
steve accounts
steve models
steve routes
steve sessions
steve usage
steve migrate
```

## Status

**M0 — clean foundation: done.** It landed on `main` in [PR #1](https://github.com/djh00t/steve/pull/1) (`cbc2c44`). `steve serve` boots with SQLite or PostgreSQL, health and management endpoints respond, and local and S3-compatible object storage share one contract.

**M1 — real proxy hot path: in progress.** Issues [#6](https://github.com/djh00t/steve/issues/6)–[#10](https://github.com/djh00t/steve/issues/10) cover OpenAI and Anthropic ingress, streaming, cancellation, upstream adapters, model listing, and provider health. Non-stream Chat Completions can forward to a configured OpenAI-compatible upstream. Other forwarding and streaming remain later M1 work.

The MVP stays a complete vertical slice. Delivery order is in the [MVP plan](docs/plans/2026-09-26-steve-mvp.md). Protocol and product boundaries are in the [architecture spec](docs/specs/2026-09-26-steve-gateway.md).

## Chat Completions forwarding

Set `server.openai_upstream_url` to an HTTP OpenAI-compatible origin (for example `http://127.0.0.1:18080` for `make test-upstream`). `POST /v1/chat/completions` forwards validated non-stream JSON and returns the upstream JSON. A completed attempt is recorded in memory and logged as `Success`; upstream failures return HTTP 502 (HTTP 504 for timeouts). Without an upstream URL, the existing HTTP 501 stub remains. Streaming still returns HTTP 501.

## Anthropic Messages ingress

`POST /v1/messages` on the inference listener (default `[::]:11435`).

```text
content-type: application/json
x-api-key: <api-key>
anthropic-version: 2023-06-01
```

A minimal body is `model`, `max_tokens`, and `messages`. `stream` is optional and defaults to `false`.

```json
{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}
```

Invalid JSON or a missing or empty required field returns HTTP 400 with the Steve error model (`error.message`, `error.type`, `error.code`, `error.param`). A valid body returns HTTP 501 with a typed stub (`request_id`, `attempt_id`, `model`, `max_tokens`, `stream`, `status`) until an upstream is connected.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Before pushing, run `make check` and `make quality-gates`.

## License

Apache-2.0 — Copyright (c) 2026 David Hooton. See [LICENSE](LICENSE).
