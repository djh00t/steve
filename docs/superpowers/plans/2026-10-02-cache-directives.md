# Cache directive preservation implementation plan

Goal: preserve safe provider response cache directives; keep experimental caching disabled until admission is qualified.
Architecture: explicit per-response metadata in existing upstream/ingress flow, with backwards-compatible body APIs. No shared mutable header state, dependencies, authentication or accounting changes. vSR admission is a separate experimental design boundary.
Spec: David selected option A on 2026-10-02 10:51:36 UTC: fix Steve header preservation, then test a pre-storage cache safeguard; bring a tested release PR before installed app changes.

## Packages (5–10 minute slices)

1. Header regression/interface: isolated child from 85e7d69; reproduce JSON/SSE/error directive loss before implementation. Safe allowlist, repeated values, Connection-nominated exclusions; preserve cancellation/no replay. Existing body APIs remain compatible.
2. Header implementation/verification: OpenAI Chat/Responses and Anthropic Messages upstream clients plus corresponding ingress replies; focused tests then full relevant check. No secrets/hop-by-hop forwarding. Independent source review.
3. Experimental cache admission assessment: pinned vSR source and executed fixture establish whether response headers can veto storage at an existing boundary. If a new subsystem/interface is required, write design and stop that implementation arm. Do not fake a custom fixture fix as an upstream fix.
4. Release integration/review: reviewed child PR into codex/release-0.2.1, conventional fix commit implies 0.2.1 after 0.2.0 candidate. One non-draft consolidated PR to main; exact-head CI, native build and fixture demo instructions. Human merges release; no installed app changes.

Review focus: repeated cache headers, malicious Connection tokens, transport failures versus provider HTTP failures, SSE cancellation/retry, synthetic identity versus authentication, unknown provider usage.
