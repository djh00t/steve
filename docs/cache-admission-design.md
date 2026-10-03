# Experimental response cache admission: approval boundary

Status: design proposal; response caching remains disabled. David approved fixing Steve header preservation and an experimental admission safeguard on 2026-10-02. This document identifies the selective safeguard that cannot be implemented through the existing configuration boundary.

## Evidence and root cause

Official vSR v0.4.0 ARM64 digest `sha256:d4e6e2f2077bbd4557faa416415fe48d00f49af7f1a58fa23414200be57323f5`, release-manifest source `6573123715e214a9eaa3ad23372ee5157411e519`, was executed with AgentGateway v1.6.0-rc.1 and a synthetic provider. Two identical JSON requests caused one upstream call and equal completion choices. Changed user/system/tenant inputs missed. A provider `Cache-Control: no-store` response was nevertheless cached: repeated requests caused one upstream call, and the cache hit omitted the directive. All Docker resources and the acquired image were removed; no real providers were called.

At that pinned source, `pkg/extproc/processor_res_cache.go:updateResponseCache` checks the existing `ctx.CacheWriteBypass` before storage. Request cache-control processing sets that flag from request headers. Response-header processing does not set it from the provider response. Downstream response transformations happen too late to prevent storage. Static suppression of all response-body processing can prevent population, but does not implement selective admission or prove existing entries cannot replay.

## Smallest selective design requiring approval

Patch the pinned upstream vSR response-header handler, not Steve's proxy and not a new sidecar. Before any buffered or reconstructed streaming response is handed to `updateResponseCache`, inspect every provider Cache-Control field, case-insensitively, respecting comma-delimited directive names and optional values. Set the existing cache-write bypass flag monotonically for no-store; never reset a request-derived bypass. For this conservative experimental cache, also refuse private/no-cache, Set-Cookie, Vary:* and malformed/ambiguous policy rather than inventing freshness or principal semantics. Existing response status, personalization, tool-history and completion checks remain required.

Only new isolated cache state may be used. Never open a previously populated unsafe cache and rely on skipping future writes. Request no-store must bypass lookup and storage; request no-cache must bypass reuse. Caller-supplied tenant headers must not grant identity: qualify authenticated server-owned identity separately before adoption. A cache hit with prohibited metadata is a test failure, not permission to erase the evidence after storage.

Acceptance: (1) safe repeat genuinely hits; (2) provider no-store/no-cache/private and repeated mixed-case directives result in two upstream calls and zero stored entries; (3) request bypass also yields no reuse/storage; (4) errors, incomplete/cancelled streams, tool calls and paired tool-history hazards cannot populate/replay; (5) unknown usage remains unknown, cache-hit accounting is distinguished from provider billing; (6) changed system/input/identity boundaries miss. Verify upstream unit storage counters plus actual pinned gateway integration on a newly built patch image. Do not label that image the unmodified official digest.

## Decision

The selective gate is an upstream behavior change outside the existing Steve/config boundary. No vSR patch, new middleware, or default caching enablement is included in the Steve header-fix release. Approval needed for this exact upstream patch/build/evaluation arm; no production installation or broad gateway adoption is implied. A stateful ext_proc middleware is larger and is not recommended.

Pinned source references: [response-header handling](https://github.com/vllm-project/semantic-router/blob/6573123715e214a9eaa3ad23372ee5157411e519/src/semantic-router/pkg/extproc/processor_res_header.go#L10), [request-only controls](https://github.com/vllm-project/semantic-router/blob/6573123715e214a9eaa3ad23372ee5157411e519/src/semantic-router/pkg/extproc/req_filter_cache_control.go#L13), [shared pre-storage gate](https://github.com/vllm-project/semantic-router/blob/6573123715e214a9eaa3ad23372ee5157411e519/src/semantic-router/pkg/extproc/processor_res_cache.go#L19), [gateway interface](https://github.com/agentgateway/agentgateway/blob/e951d942e638326bef679fd0132884fb4169820e/crates/agentgateway/src/http/ext_proc.rs#L272). These are source findings; the executed official digest has no independently verified source/revision attestation.
