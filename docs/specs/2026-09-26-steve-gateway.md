# Steve Gateway — Architecture and Product Specification

Date: 2026-09-26
Status: Baseline for implementation

## 1. Product definition

Steve is a portable, open-source, multi-user LLM gateway and control plane.

Primary use cases:

1. **Developer local install** — one developer wants one endpoint, one place for API keys and routing, accurate usage/cost reporting, and a native macOS experience.
2. **Home gateway** — several people share a gateway while retaining separate identities, provider accounts, usage, chat histories, quotas, and access policy.
3. **Small/medium business gateway** — centralised model access, provider/account pools, policy, chargeback, session history, routing, reliability telemetry, and enterprise observability.

The same Rust daemon and protocol contracts serve all three profiles. Deployment scale changes the backing infrastructure, not the application architecture.

## 2. Runtime and deployment

Steve must support:

- native Rust binaries;
- OCI containers on Linux, Windows, and macOS;
- Apple Container, Docker, Podman, and Kubernetes-compatible runtimes;
- remote deployments on a home server, VM, or business cluster.

The macOS Swift app is optional. It is a control surface, not a required runtime.

First-run macOS discovery:

1. Check configured remote deployment.
2. Check local Steve management endpoint.
3. Check native Steve service.
4. Check supported local container runtimes.
5. If no daemon is found, offer:
   - Install native
   - Run in local container
   - Connect to existing proxy

A remote configuration may use a lightweight local forwarding mode so development tools can continue to target a stable localhost URL.

## 3. Interfaces

### Inference APIs

MVP:
- OpenAI Responses API compatibility.
- OpenAI Chat Completions compatibility.
- Anthropic Messages compatibility.
- Server-sent event streaming.
- Model listing.
- Health endpoint.

Provider-specific pass-through should be preferred when ingress and egress protocols match. Translation should occur only when needed.

### Management API

The management API is the canonical control plane for all UIs. It covers:

- deployments and system status;
- users, clients, organisations;
- providers and accounts;
- model catalogue and capabilities;
- pricing and FX;
- profiles and routes;
- sessions, messages, requests and attempts;
- costs, usage, latency and errors;
- storage and retention;
- observability settings;
- audit events.

### UIs

- **CLI**: scriptable administration and diagnostics.
- **TUI**: eventually 100% of daemon configuration and reporting.
- **Web UI**: full graphical administration after the initial MVP.
- **Swift menu-bar app**: onboarding, status, account/provider usage, deployment switching, common routing-profile changes, and links to full administration.

## 4. Core domain model

The canonical entities are:

- Organisation
- User
- Client
- Deployment
- Provider
- UpstreamAccount
- AccountBinding
- AccountPool
- Model
- ProviderModel
- PricingVersion
- FXRate
- Profile
- RouteRule
- Session
- Turn
- Message
- Request
- RequestAttempt
- RoutingDecision
- LatencySpan
- UsageCharge
- ErrorEvent
- ObjectRef
- AuditEvent

### Provider versus account

A Provider is a service definition such as Anthropic, OpenAI, OpenRouter, DeepSeek, or GitHub Copilot.

An UpstreamAccount is an actual credential/subscription/billing account for that provider.

Example:

```text
Provider: Anthropic

Accounts:
- David Anthropic
- Alice Anthropic
- Bob Anthropic
- Shared API account
```

Each account owns independent authentication, ownership, allowed users, health, quota, rate-limit state, pricing context, latency history, error history, and priority.

Account pools support policies such as:

- dedicated;
- prefer-own-account;
- weighted round robin;
- least utilised;
- lowest latency;
- lowest cost;
- highest remaining quota;
- failover.

Affinity options include none, user-sticky, and session-sticky.

Eligibility/access filtering happens before routing scores are calculated.

## 5. Identity and attribution

Every request should be attributable, where possible, to:

```text
Organisation
  -> User
    -> Client
      -> Session
        -> Turn
          -> Request
            -> RequestAttempt[]
```

Client identities may represent a human-facing application, IDE, CLI, autonomous agent, service account, or automation.

All accounting, errors, access checks, latency, history, and audit views must preserve this attribution.

## 6. Sessions and chat history

Steve owns its canonical session identity independently of any provider.

A conversation may use multiple models/providers while remaining one Steve session.

Session association priority:

1. Explicit Steve session ID.
2. Client/provider supplied conversation identifier.
3. Protocol-native continuation identifier.
4. Correlation heuristic.
5. New session.

Association quality must be recorded as exact, inferred, or unknown.

Steve should expose an `X-Steve-Session-ID` header and accept equivalent structured metadata where protocols allow it.

MVP tracks sessions and messages. Transparent cross-provider reconstruction of stateful server-side conversations is post-MVP unless the full conversation is already present in the request.

## 7. Requests and attempts

A logical request can generate multiple upstream attempts.

Example:

```text
Request
  route -> Claude-capable target

Attempt 1 -> Anthropic account A -> 429
Attempt 2 -> Anthropic account B -> network failure
Attempt 3 -> OpenRouter account -> success
```

Attempts independently record provider, account, model, request timing, status, errors, token usage, and cost.

Once streamed output has been delivered to the client, automatic retry/failover must not replay the generation silently.

## 8. Routing architecture

Routing is separated into two decisions.

### Model/task selection

Determines the logical model/tier/capability appropriate for the request.

Sources may include:

- explicit model;
- alias/profile;
- deterministic rules;
- capability requirements;
- context length;
- Switchyard;
- later specialist evaluators such as Jev.

Switchyard must sit behind Steve's own `RoutingEngine` interface so it can be upgraded or replaced without changing the public architecture.

### Provider/account selection

For the selected logical model/capability, determine the eligible provider account.

Inputs include:

- access policy;
- account ownership/pool eligibility;
- model capability;
- context support;
- health;
- remaining quota/rate limits;
- canonical USD price;
- measured TTFT and total latency;
- recent error/429/5xx rates;
- configured affinity;
- routing profile.

Initial algorithms should be deterministic and explainable.

Every routing decision is persisted with candidates, inputs, scores, exclusions, chosen target, and reason.

## 9. Latency instrumentation

Measure timestamps/spans through the entire proxy hot path, including:

- ingress;
- authentication;
- user/client resolution;
- session resolution;
- model routing;
- account/provider selection;
- request transformation;
- upstream connection;
- response headers;
- first byte;
- first token;
- final token;
- response transformation;
- persistence/accounting;
- total request duration.

Each upstream attempt has its own timing data.

Aggregates should include p50/p95 and short/medium-window EWMAs by provider, account, model, deployment region where meaningful.

Latency and reliability measurements are usable as routing inputs.

## 10. Pricing and currencies

Every ProviderModel/UpstreamAccount combination must have a pricing representation, even when pricing mode is not ordinary token metering.

Pricing modes:

- metered;
- subscription;
- quota;
- free;
- unknown.

Preserve:

1. authoritative source/list price and currency;
2. canonical normalised USD value;
3. optional user-selected display currency.

Pricing records must retain:

- source currency;
- source unit;
- source amount;
- canonical USD amount;
- FX rate used;
- FX timestamp/source;
- effective date;
- retrieval date;
- pricing source/provenance.

For historical requests, persist the price version and FX rate used at accounting time. Do not recompute historical costs using current FX.

The UI shows local display currency as the primary value where configured, with canonical USD price and FX rate visible as secondary text.

Pricing must be flexible enough for:

- input/output tokens;
- cache reads/writes;
- reasoning tokens;
- images;
- audio/video;
- tool/search/computer-use charges;
- batch prices;
- context tiers;
- subscription allowances and overages.

## 11. Providers and upstream protocols

Initial provider presets should cover:

- OpenAI API
- Anthropic API
- Google Gemini
- xAI/Grok API
- DeepSeek
- Groq
- Cerebras
- Mistral
- OpenRouter
- generic OpenAI-compatible
- generic Anthropic-compatible
- LM Studio/Ollama/local-compatible endpoints

Additional planned integrations:

- GitHub Copilot subscription/OAuth
- ChatGPT/Codex subscription authentication
- Vercel AI Gateway
- LiteLLM
- Portkey
- AWS Bedrock
- Azure OpenAI / Microsoft Foundry
- Google Vertex AI
- NVIDIA NIM
- vLLM
- Jev specialist decision/evaluation integration

Provider, protocol, authentication mechanism, and model capability must remain separate abstractions. Most provider presets should be declarative mappings over shared protocol/auth implementations.

Unofficial subscription bridges must be clearly marked and must not be allowed to destabilise the core provider architecture.

## 12. Persistence

### Relational source of truth

Tier 1 databases:

- SQLite for zero-admin/local/home installs.
- PostgreSQL for larger/multi-instance installs.

MySQL/MariaDB should remain architecturally possible but is not required for MVP.

Relational storage is authoritative for:

- identities/access;
- configuration;
- provider accounts;
- model/pricing catalogue;
- sessions/messages metadata;
- requests/attempts;
- routing decisions;
- usage/cost accounting;
- latency summary/index data;
- errors;
- object references;
- audit trail.

SQLite should use WAL mode and sensible retention/rollups.

### Object storage

Object storage supports:

- local filesystem;
- S3-compatible storage.

S3-compatible targets may include AWS S3, MinIO, Garage, Ceph RGW, R2, Backblaze B2, and similar implementations.

Use it for potentially large content:

- chat message bodies;
- raw inbound/upstream request bodies;
- raw responses;
- streaming event captures;
- tool calls/results;
- optional diagnostic payloads.

Relational rows retain searchable metadata and object references.

Retention and content logging must be configurable independently. Prompt/response content capture should be explicitly controllable and suitable for being disabled by privacy policy.

## 13. Observability

Steve emits structured application logs and OpenTelemetry traces/metrics/logs.

Built-in management reporting remains functional without external observability.

Large installs may connect OTLP to Grafana-compatible infrastructure such as:

- Prometheus/Mimir for metrics;
- Loki for logs;
- Tempo for traces;
- Grafana for visualisation.

External telemetry is not authoritative for accounting or configuration.

Useful metrics include:

- request totals;
- input/output/cache token totals;
- canonical USD cost;
- request/TTFT latency histograms;
- active requests;
- provider/account health;
- errors, 429s, retries, failovers;
- stream disconnects;
- route-decision counts.

Avoid high-cardinality metric labels such as request IDs, prompt text, or user email.

## 14. Security

- Provider credentials are never exposed to clients.
- Access to an upstream account is evaluated before routing.
- Local daemon defaults to loopback binding.
- Remote deployment requires authenticated TLS.
- Secrets storage is deployment-specific; macOS native can use Keychain, while server/container deployments use injected secrets or supported secret stores.
- Prompt/response logging is policy-controlled.
- Configuration/security changes generate audit events.
- Future SMB features include OIDC/SSO, RBAC, mTLS, and enterprise secret-store integration.

## 15. Non-goals for MVP

- Perfect compatibility with every LLM provider.
- Transparent reconstruction of every provider's proprietary server-side conversation state.
- Full enterprise IAM.
- HA/multi-region control plane.
- Custom data warehouse.
- Proprietary model evaluation framework.
- Reimplementing Switchyard or observability stacks.

## 16. Engineering principles

- Smallest complete vertical slice first.
- Keep the daemon modular but avoid speculative plugin frameworks.
- Reuse maintained libraries where they eliminate real complexity.
- Preserve provider-native features when pass-through is possible.
- Prefer deterministic, inspectable behaviour for the first routing implementation.
- Make every automatic routing choice explainable.
- Keep business/accounting data separate from high-volume telemetry.
- No dependency name or implementation detail becomes part of Steve's public identity.


## 17. Hot-path isolation and deferred work

The forwarding and UI/API hot paths are latency-critical. Steve must not synchronously wait for work that is not required to produce the immediate response.

### Hot-path rule

A request may synchronously wait only for work required to safely select and execute an upstream request, including:

- authentication/access checks needed to authorize the call;
- session/account/routing state required for the current decision;
- provider selection;
- protocol transformation required for forwarding;
- the provider call and streamed response itself.

The hot path must not wait for:

- telemetry export;
- log shipping;
- analytics aggregation;
- usage rollups;
- chat-history/object-store writes;
- nonessential audit enrichment;
- pricing refresh;
- FX refresh;
- report generation;
- Grafana/OTLP delivery;
- UI cache refresh.

### Deferred-work architecture

Noncritical side effects are published to bounded in-process queues and consumed by background workers.

Requirements:

- use Tokio async tasks and nonblocking channels;
- never use an unbounded queue;
- hot-path producers prefer `try_send`/equivalent over awaiting queue capacity;
- queue pressure is observable;
- work classes have independent queues/budgets so slow object storage cannot starve accounting or telemetry;
- critical accounting/audit events may spill to a local durable journal when their queue is saturated rather than blocking forwarding;
- lossy telemetry may be sampled or dropped under pressure, with explicit dropped-event counters;
- background workers use bounded concurrency, timeouts, retries, and circuit breakers;
- CPU-heavy serialization/compression/cryptography must use bounded blocking/worker pools rather than occupying async reactor threads;
- database connection pools for request-critical reads and background writes are isolated or reserved so background work cannot exhaust the hot-path pool;
- management/UI endpoints use independent concurrency limits from inference forwarding.

The intended topology is:

```text
request/stream hot path
  |
  +--> immediate upstream forwarding
  |
  +--> try_enqueue accounting --------> durable/background accounting worker
  +--> try_enqueue history -----------> object-store worker
  +--> try_enqueue telemetry ---------> OTLP/log worker
  +--> try_enqueue aggregation -------> rollup worker
```

Backpressure must degrade observability/history before it degrades forwarding. Authoritative accounting/audit data must not be silently lost: when it cannot be queued, Steve must use a durable local fallback journal and reconcile it asynchronously.

### Cached routing state

Routing must not run analytical database queries per request when avoidable.

Maintain in-memory snapshots/caches for:

- provider/account eligibility;
- model capabilities;
- price tables;
- FX tables;
- quota/rate-limit state;
- rolling latency/reliability summaries;
- active profiles/routes.

Background tasks refresh those snapshots. Routing reads immutable/current snapshots with minimal locking and records the version used in each decision.

## 18. Lifecycle, draining, and hitless upgrades

Steve is designed for hitless or near-hitless upgrades.

A process has explicit lifecycle states:

```text
starting -> ready -> draining -> stopped
```

Readiness is distinct from liveness:

- liveness means the process is functioning;
- readiness means it may accept new inference work;
- a draining worker remains live but becomes unready immediately.

Steve tracks active requests/streams. During drain:

1. stop assigning new requests to the old worker;
2. keep all existing HTTP/SSE streams alive;
3. finish queued critical accounting/audit work or hand it to the replacement worker/durable journal;
4. flush bounded state required for correctness;
5. terminate only when active work reaches zero or the configured maximum drain deadline expires.

### Upgrade coordinator

The upgrade design must not require the serving worker to replace itself in-place.

Use a small Steve supervisor/coordinator mode for managed native installs:

```text
stable client endpoint/listener
        |
   Steve supervisor
      /       \
 old worker  new worker
 draining      ready
```

The supervisor owns or mediates the stable endpoint and worker lifecycle. An upgrade:

1. downloads the new signed release to a versioned path;
2. verifies checksum/signature and compatibility;
3. starts the new worker with the same external configuration;
4. waits for migrations/compatibility checks and readiness;
5. atomically directs new work to the new worker;
6. marks the old worker draining;
7. waits for its active streams/tasks to complete;
8. terminates the old worker;
9. rolls back routing to the old worker automatically if the new worker fails before cutover completes.

Container/Kubernetes deployments use the same readiness/drain semantics but normally delegate process replacement to the container orchestrator. Steve must support rolling upgrades with multiple replicas and must not depend on in-process self-update in that environment.

Upgrade policy can be:

- manual;
- scheduled;
- automatic within an allowed maintenance window.

Database migrations required for hitless upgrades must follow expand/contract compatibility: a new release must be able to operate while the previous release is still draining. Destructive schema changes require a later cleanup release after all old workers are gone.

Native macOS/Windows/Linux packaging can implement platform-specific listener handoff underneath the same supervisor contract. The public management API and lifecycle semantics remain cross-platform.
