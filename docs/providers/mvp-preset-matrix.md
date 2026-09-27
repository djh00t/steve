# MVP provider preset inventory

This inventory records the ten required preset names from the approved [MVP plan](../plans/2026-09-26-steve-mvp.md#MVP-provider-presets). It assigns protocol-review packets and evidence sets; it does not select protocols, auth, endpoints, models, or features. Every such claim remains **unresolved** until the assigned packet records reviewed evidence and acceptance.

| Required preset (approved name) | Protocol review owner | Implementation leaf | Source set for review | Protocol / auth / endpoint / model / feature status |
| --- | --- | --- | --- | --- |
| OpenAI API | #383 | #400 | OpenAI first-party API reference, auth, models, streaming docs; relevant SDK docs, samples, or fixtures where available; record gaps | Unresolved pending #383 |
| Anthropic API | #399 | #403 | Anthropic first-party API reference, auth, models, streaming docs; relevant SDK docs, samples, or fixtures where available; record gaps | Unresolved pending #399 |
| Gemini | #384 | #401 | Google first-party API reference, auth, models, streaming docs; relevant SDK docs, samples, or fixtures where available; record gaps | Unresolved pending #384 |
| xAI/Grok API | #385 | #402 | xAI first-party API reference, auth, models, streaming docs; relevant SDK docs, samples, or fixtures where available; record gaps | Unresolved pending #385 |
| DeepSeek | #386 | #404 | DeepSeek first-party API reference, auth, models, streaming docs; relevant SDK docs, samples, or fixtures where available; record gaps | Unresolved pending #386 |
| Groq | #387 | #405 | Groq first-party API reference, auth, models, streaming docs; relevant SDK docs, samples, or fixtures where available; record gaps | Unresolved pending #387 |
| OpenRouter | #388 | #406 | OpenRouter first-party API reference, auth, models, streaming docs; relevant SDK docs, samples, or fixtures where available; record gaps | Unresolved pending #388 |
| generic OpenAI-compatible | #389 | #407 | OpenAI-compatible reference and relevant SDK docs, samples, or fixtures where available; record gaps; reviewed generic endpoint, auth, model, and streaming evidence | Unresolved pending #389 |
| generic Anthropic-compatible | #390 | #408 | Anthropic-compatible reference and relevant SDK docs, samples, or fixtures where available; record gaps; reviewed generic endpoint, auth, model, and streaming evidence | Unresolved pending #390 |
| local OpenAI-compatible endpoint (covers LM Studio/vLLM/NIM-style deployments where compatible) | #389 | #409 | OpenAI-compatible reference and relevant SDK docs, samples, or fixtures where available; record gaps; local server documentation and fixture evidence for endpoint, auth, models, and streaming | Unresolved pending #389 and local evidence |

The generic OpenAI-compatible and local OpenAI-compatible rows are distinct presets. Shared review packet #389 does not imply compatibility or behavior for either row; local evidence must qualify the local endpoint case.

## Boundaries and follow-up

- This is an inventory mapping only. The approved local plan is the source for the preset list; no runtime or policy adoption is implied. Experimental and deferred integrations remain outside this required set, and none may block the core MVP.
- #410 owns the reusable fixture scaffold and preset scenario table. Protocol packets should cite concrete, versioned first-party documentation and relevant SDK documentation and samples or fixtures where available, recording gaps; unresolved evidence stays unresolved.
- Preset implementation remains blocked on its review packet and the provider account/credential contract (#100), HTTPS transport decision (#132), and provider auth-header/credential-isolation proof (#398), plus the listed implementation prerequisites. Re-size implementation leaves after accepted reviewed evidence exists and before marking any runtime work READY.
- One writer owns this shared matrix at a time. Serialize edits to it across packages; separate files may proceed independently.
- Inventory mapping acceptance: David/Cos. This acceptance does not approve protocol behavior, policy, or runtime adoption.
