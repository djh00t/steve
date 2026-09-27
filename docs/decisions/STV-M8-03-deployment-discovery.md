# STV-M8-03 deployment discovery and safe actions — proposal

**Issue:** [#337](https://github.com/djh00t/steve/issues/337)
**Status:** Proposal only; pending David/Cos review and explicit acceptance of an exact revision.
**Scope:** macOS controller discovery of a configured remote, a local Steve endpoint/native service, Apple Container, and Docker. This note authorizes no runtime action, probe, implementation, or deployment.

**Native consumer prerequisite:** After #336 is accepted, #341 defines the exact launchd service identifier/domain and package receipt contract; its implementation children provide installed evidence. #345 stays blocked until those values are delivered and qualified; they are not a prerequisite for accepting this discovery policy.

## Decision

Keep discovery read-only and order candidates by the gateway specification:

| Order | Candidate | Probe and positive evidence | What a negative result means |
|---|---|---|---|
| 1 | Configured remote | Probe only the explicitly selected remote profile using `GET /health/ready`, M2 authentication, verified TLS, no redirects, and a 1-second request deadline. A valid Steve health payload distinguishes `ready` from `not_ready`. | Preserve authentication, TLS, timeout, and network failures separately. Do not scan other saved remotes or guess hosts. |
| 2 | Local Steve endpoint | Probe the configured local management address; if unset, probe only IPv6/IPv4 loopback on the current default management port `8790`. Request `/health/ready` and `/api/v1/system/status`; do not scan ports. | Refusal means no listener at that address, not proof that Steve is absent. A responding non-Steve or unrecognized endpoint is `unknown`. |
| 3 | Native Steve service | Inspect only the exact launchd service identifier, domain, and installation receipt/path defined by #341 under accepted #336 and evidenced by its qualified installation children, using read-only status queries. Every existing Steve app/daemon component must match the approved identity; absent components need no signature. Cross-check process readiness through the local Steve endpoint. | Report `absent` only when every exact service and receipt/path probe succeeds and finds no installation evidence. Any probe error or incomplete result is `unknown`, not absence. Installed but unloaded/stopped means `installed_stopped`. Do not search or infer from process names. If #341 and its installation children have not delivered the qualified service identity/domain, report `unknown`; do not dispatch #345. |
| 4 | Apple Container | On macOS, resolve the `container` CLI and run `container system status`; determine success from its exit status. Only after success, run `container list --all --format json` and batch `container inspect` for listed IDs. Match only the exact label `io.steve.deployment=steve-gateway`; read its runtime state independently of Steve readiness. | CLI missing, nonzero status exit, or denied access means the runtime is unavailable/unknown, not `Steve absent`. `absent` requires a successful, complete inventory with no matching label. |
| 5 | Docker | Resolve the `docker` CLI and inspect local context metadata without contacting an engine. Probe only a context whose endpoint is a local Unix socket; do not contact `ssh://`, `tcp://`, or other remote contexts. List all states with `docker --host <inspected-local-endpoint> container ls --all --filter label=io.steve.deployment=steve-gateway --format json`; run `docker --host <inspected-local-endpoint> container inspect <matching-ids>` for exact label and state. | A successful complete list with no matching label means `Steve absent`. CLI missing, engine unreachable, permission denied, and remote-only context remain distinct from absence. |

Probes may run concurrently, but display and candidate priority remain in the table order. Preserve the user's existing selected target. On first run, highlight the first `ready` Steve candidate in that order; if multiple instances tie at the same priority, require the user to choose. Highlighting never saves a target or connects automatically.

## Result semantics and bounds

- Keep **runtime client**, **runtime engine/service**, **Steve deployment**, and **Steve readiness** as separate observations. An installed Docker or Apple Container runtime does not establish that a Steve container exists or is running.
- Use `absent` only after a successful complete endpoint/inventory check or successful completion of every exact native service and receipt/path probe with no installation evidence. Use `unreachable` for refusal, network failure, or timeout; preserve `authentication_failed`, `tls_error`, and `permission_denied` where applicable. Use `unknown` for probe errors that cannot be classified, malformed/unrecognized responses, truncated inventory, or an unsupported service identity. Never turn any of these into `absent`.
- An exact Steve health response with `ready` is `ready`; `not_ready` or a lifecycle phase such as `starting`/`draining` is `running_not_ready`. Container/service state alone proves only process/deployment presence.
- Bound the configured remote HTTP probe to **1 second** (the stricter exception); bound other HTTP requests and child processes to **2 seconds**, with a **10-second total discovery deadline**, no retries, and a **64 KiB output cap per child process**. Kill a child at its deadline. Truncated or over-limit inventory is `unknown`, never `absent`.
- Container identity is the exact label `io.steve.deployment=steve-gateway`; container names, image names, ports, or any arbitrary running container are not identity. Labels nominate a candidate; the Steve health endpoint confirms readiness. Multiple matching containers remain separate candidates.
- Do not expose endpoint credentials, response bodies, container environment, or full inspect output in UI/logs. Remote readiness uses M2 auth/TLS and redaction. Every Docker engine query must explicitly pin the inspected local endpoint with `--host` and remove `DOCKER_HOST`/`DOCKER_CONTEXT` overrides; never fall back to the active/default context after inspection. Explicit remote Docker contexts are outside this local probe.

## Confirmation policy

1. Opening onboarding and running any discovery probe has **zero mutation**: no target switch/save, launch/start/stop/restart/drain, container create/start/stop, image pull, install, update, or forwarding listener.
2. **Connect/select** requires the user's explicit selection. It may save that target and use the accepted M2 authentication/TLS contract; it never starts or changes the remote deployment. Starting a local forwarding listener is a separate action and confirmation.
3. **Start** requires an explicit confirmation naming the selected deployment and runtime, followed by one start of that existing service/container only. Never install, create, pull, or start another candidate as fallback. Poll readiness only to the consumer's accepted deadline; timeout leaves the started target visible as not ready and does not trigger retries.
4. **Stop** requires a separate confirmation naming the target and warning that it interrupts service. Use the accepted Steve drain/lifecycle path before the runtime stop. If graceful stop is unsupported or fails, report the error and do not force-kill.
5. **Update** is never automatic or part of discovery. For native deployments, use the flow accepted by #336. One explicit confirmation names current and target versions and authorizes staging, signature/checksum verification, activation, readiness checks, and automatic rollback if the candidate fails before cutover completes. For a managed local container, #377 must qualify a runtime/orchestrator-owned replacement flow with explicit current and target image identity, image verification, readiness/drain and failure behavior before its update control is enabled; the container confirmation must name the selected deployment, runtime, and current/target image identities; it authorizes only that selected deployment. The serving worker never replaces itself. Do not schedule/background updates, manipulate unrelated containers, or bypass the qualified owner/orchestrator.
6. **Install** is a distinct explicit user action after the accepted #336 source/trust decision. Discovery cannot download or install software.

## Negative cases

- Docker/Apple Container engine running with no matching Steve label → runtime available, Steve absent.
- Steve label found on a stopped container → deployment installed/stopped, readiness unknown; no auto-start or pull.
- A random running container, Steve-looking name/image without the label, or a health endpoint that is not a Steve response → do not report Steve ready.
- Local port refusal, occupied port serving another program, permission denial, bad remote certificate, 401/403, malformed health JSON, timeout, or truncated inventory → preserve the corresponding error/unknown state; never offer a duplicate install as if Steve were proven absent.
- Multiple ready remotes/instances → preserve configured/user choice; require explicit selection and do not switch to the first response to finish.
- Apple `container system start` and `container system stop`, Docker start/stop, image pull, install, and update are mutation commands: none may run during discovery.
- Podman, Kubernetes/kubectl, cluster enumeration, and Kubernetes UI are unsupported by this decision.

## Evidence and downstream gates

- The gateway specification fixes first-run ordering and says the Swift app is optional: [`docs/specs/2026-09-26-steve-gateway.md`](../../docs/specs/2026-09-26-steve-gateway.md#L18).
- Current Steve source exposes `/health/ready`, `/api/v1/system/status`, and `/api/v1/system/version`; readiness has a separate `ready`/`not_ready` state: [`src/server.rs`](../../src/server.rs#L344).
- Apple's maintained CLI reference documents `container system status` as a health-check request, `container list --all --format json`, JSON inspect, and separate mutating system start/stop commands. Determine status success from the command exit status; do not parse status output: [Apple Container command reference](https://github.com/apple/container/blob/main/docs/command-reference.md). These CLI details were checked against the upstream reference on 2026-09-28.
- Docker documents that contexts can point to remote endpoints and `DOCKER_CONTEXT` overrides the selected context; the CLI can list containers including stopped ones and filter by labels: [Docker contexts](https://docs.docker.com/engine/manage-resources/contexts/), [Docker container CLI](https://docs.docker.com/reference/cli/docker/container/), [Docker `container ls`](https://docs.docker.com/reference/cli/docker/container/ls/). These CLI details were checked against official docs on 2026-09-28.
- Repository evidence supports a native binary, OCI runtimes, optional controller, and remote deployments. The health/status routes exist today; native install/service identity and platform UI do not. No live probe or runtime result is claimed.

Consumers stay blocked until David/Cos accepts an exact artifact revision and each owner updates/re-sizes its brief against it:

| Consumer | Gate from this decision |
|---|---|
| #342 / M8-06 discovery value model | Keep runtime presence, Steve installation/deployment, endpoint, and readiness distinct; represent absent, unreachable, denied, and unknown without collapsing them. |
| #343 / M8-07 remote probe | Use only configured remote + M2 auth/TLS and preserve auth/TLS/network/timeout outcomes; redact credentials. |
| #344 / M8-08 local probe | Use configured/default loopback only; query current health/status contract; distinguish no listener, another service, and Steve not ready. |
| #345 / M8-09 native service probe | Wait for #341's accepted service identity/domain contract and its installation children's qualified evidence under accepted #336; read-only status only. |
| #346 / M8-10 container runtime probe | Support only Apple Container and local Docker; use exact label; do not pull/start; distinguish engine from Steve state. |
| #348 / M8-11 onboarding | Keep selection explicit, preserve saved target, and render discovery with zero mutations. |
| #340 / M8-29, then #372 / M8-24 | #340 remains a disconnected UI shell with no discovery; after all probe consumers land, #372 verifies precedence, errors, one selected path, and no startup/view mutations using deterministic fakes. |
| #341 / native installation, after #336 acceptance | Define the exact native service identity/domain and receipt contract; installation children supply qualified installed evidence before #345 proceeds. The discovery-policy proposal does not wait for installation implementation. |

Review evidence is this decision table, negative cases, cited official/runtime source facts, and consumer gates against acceptance example STV-M8-03-D1. No implementation test is required or claimed by this proposal.
