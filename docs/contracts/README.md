# Contract registry

This index reflects the 2026-09-27 backlog snapshot. Paths are **planned artifacts unless their row records a proposal or accepted revision**; a present proposal is not approval. It points to producer issues and canonical artifact paths; accepted artifacts and code/migrations are authoritative, and this page does not define schemas. **READY** on a decision producer permits preparing a proposal. It is not approval and does not unblock implementation. Use the accepting authority named by each producer issue; where none is recorded, do not infer one. Record the responsible authority and explicit acceptance with the contract before dispatching consumers.

| Producer | Contract outcome | Canonical artifact | Status / prerequisites |
| --- | --- | --- | --- |
| [STV-M0-01 (#84)](https://github.com/djh00t/steve/issues/84) | Coordinate accounting failure contracts | Child artifacts below; former umbrella draft is unaccepted | COORDINATION: #483/#484/#485 plus consumer handoff |
| [STV-M0-16 (#483)](https://github.com/djh00t/steve/issues/483) | Incident/admission state, HTTP/status contract and incident lifetime | [Incident/admission proposal](stv-m0-16.md) | PROPOSED: restart/disposition decision pending David/Cos acceptance; implementation remains blocked |
| [STV-M0-17 (#484)](https://github.com/djh00t/steve/issues/484) | Journal ownership and replacement overlap | [Ownership proposal](stv-m0-17.md) | PROPOSED: David/Cos acceptance and platform qualification pending; runtime consumers blocked |
| [STV-M0-18 (#485)](https://github.com/djh00t/steve/issues/485) | Drain/replay acknowledgement and completion evidence | `docs/contracts/stv-m0-18.md` | BLOCKED on accepted #483/#484 artifacts |
| [STV-M0-09 (#90)](https://github.com/djh00t/steve/issues/90) | Independent admission budgets | [Admission proposal](stv-m0-09.md) | Accepted via [#466](https://github.com/djh00t/steve/pull/466) at `63f9cce`; implementation leaves #468-470 merged, composed #92 remains separate |
| [STV-M1-04 (#94)](https://github.com/djh00t/steve/issues/94) | Chat accounting event identity, payload and emission cardinality | `docs/m1-acceptance.md` (contract note only) | BLOCKED by STV-M0-01 (#84) |
| [STV-M2-01 (#99)](https://github.com/djh00t/steve/issues/99) | Coordinate local identity and client authentication | [Identity contract index](stv-m2-01.md) | COORDINATION: child acceptance and consumer handoffs |
| [STV-M2-60 (#500)](https://github.com/djh00t/steve/issues/500) | Principal IDs, relationships and legacy attribution | [Principal proposal](stv-m2-60.md) | PROPOSED: David/Cos exact-revision acceptance and executable contract evidence pending; #99 and runtime consumers remain gated |
| [STV-M2-61 (#501)](https://github.com/djh00t/steve/issues/501) | Local bootstrap and client credential lifecycle | `docs/contracts/stv-m2-61.md` | BLOCKED on accepted #500/#106 |
| [STV-M2-62 (#502)](https://github.com/djh00t/steve/issues/502) | Inference auth wire and resolved identity | `docs/contracts/stv-m2-62.md` | BLOCKED on accepted #500/#501 |
| [STV-M2-02 (#100)](https://github.com/djh00t/steve/issues/100) | Provider accounts, credentials, access and bindings | `docs/contracts/stv-m2-02.md` | BLOCKED by STV-M2-01 |
| [STV-M2-03 (#101)](https://github.com/djh00t/steve/issues/101) | Account-pool policy, routing and affinity | `docs/contracts/stv-m2-03.md` | BLOCKED by STV-M2-02 |
| [STV-M2-14 (#106)](https://github.com/djh00t/steve/issues/106) | Listener exposure and management authentication | [Listener/auth proposal](stv-m2-14.md) | PROPOSED: David/Cos exact-revision acceptance pending; downstream implementation remains blocked |
| [STV-M2-46 (#110)](https://github.com/djh00t/steve/issues/110) | Security audit event | [Work packages](stv-m2-46.md), [Envelope proposal](stv-m2-46-envelope.md), [Inventory proposal](stv-m2-46-inventory.md) | PROPOSED: joint #513/#514 revision acceptance, identity bindings and executable contract fixtures pending; #515 blocked on accepted #483/#484/#485; #123 remains blocked |
| [STV-M3-40 (#102)](https://github.com/djh00t/steve/issues/102) | Migration versioning and backend parity | [Migration proposal](stv-m3-40.md) | PROPOSED: David/Cos acceptance pending; #103/#104 remain blocked |
| [STV-M3-01 (#116)](https://github.com/djh00t/steve/issues/116) | Session association and conversation IDs | `docs/contracts/stv-m3-01.md` | BLOCKED by STV-M2-01 |
| [STV-M3-02 (#121)](https://github.com/djh00t/steve/issues/121) | Content capture, metadata-only mode and retention | `docs/contracts/stv-m3-02.md` | BLOCKED by STV-M3-01 |
| [STV-M4-01 (#164)](https://github.com/djh00t/steve/issues/164) | Money precision and rounding | `docs/contracts/stv-m4-01.md` | READY: proposal only |
| [STV-M4-02 (#165)](https://github.com/djh00t/steve/issues/165) | FX source and stale-rate policy | `docs/contracts/stv-m4-02.md` | READY: proposal only |
| [STV-M4-03 (#166)](https://github.com/djh00t/steve/issues/166) | Usage-price units and tier semantics | `docs/contracts/stv-m4-03.md` | READY: proposal only |
| [STV-M4-33 (#238)](https://github.com/djh00t/steve/issues/238) | Display-currency preference | `docs/contracts/stv-m4-33.md` | BLOCKED by STV-M4-01, STV-M4-02 |
| [STV-M5-01 (#174)](https://github.com/djh00t/steve/issues/174) | Latency stages and percentiles | `docs/contracts/stv-m5-01.md` | READY: proposal only |
| [STV-M5-02 (#175)](https://github.com/djh00t/steve/issues/175) | EWMA and reliability rules | `docs/contracts/stv-m5-02.md` | READY: proposal only |
| [STV-M5-03 (#176)](https://github.com/djh00t/steve/issues/176) | Normalized error taxonomy | `docs/contracts/stv-m5-03.md` | READY: proposal only |
| [STV-M5-04 (#177)](https://github.com/djh00t/steve/issues/177) | OpenTelemetry MVP boundary | `docs/contracts/stv-m5-04.md` | READY: proposal only |
| [STV-M6-01 (#186)](https://github.com/djh00t/steve/issues/186) | Deterministic routing score | `docs/contracts/stv-m6-01.md` | READY: proposal only |
| [STV-M6-02 (#187)](https://github.com/djh00t/steve/issues/187) | Fixed/rule routing precedence | `docs/contracts/stv-m6-02.md` | READY: proposal only |
| [STV-M6-26 (#270)](https://github.com/djh00t/steve/issues/270) | Immutable routing snapshot | `docs/contracts/stv-m6-26.md` | BLOCKED by STV-M4-04, STV-M5-01, STV-M2-09/07/41, STV-M6-02 |
| [STV-M6-35 (#272)](https://github.com/djh00t/steve/issues/272) | Switchyard adapter/dependency boundary | `docs/contracts/stv-m6-35.md` | BLOCKED by STV-M6-03 |
| [STV-M6-42 (#273)](https://github.com/djh00t/steve/issues/273) | Profile/alias/rule management API | `docs/contracts/stv-m6-42.md` | BLOCKED by STV-M6-02, STV-M6-04 |
| [STV-API-01 (#296)](https://github.com/djh00t/steve/issues/296) | Model catalogue management API | `docs/contracts/stv-api-01.md` | BLOCKED by STV-M2-14, STV-M6-02 |
| [STV-API-06 (#290)](https://github.com/djh00t/steve/issues/290) | Account health query API | `docs/contracts/stv-api-06.md` | BLOCKED by STV-M2-02, STV-M2-14 |
| [STV-API-08 (#318)](https://github.com/djh00t/steve/issues/318) | Storage/retention management API | `docs/contracts/stv-api-08.md` | BLOCKED by STV-M3-02, STV-M2-14 |
| [STV-API-14 (#298)](https://github.com/djh00t/steve/issues/298) | Pricing, FX and usage-report API | `docs/contracts/stv-api-14.md` | BLOCKED by STV-M4-01/02/03, STV-M2-14 |
| [STV-API-20 (#319)](https://github.com/djh00t/steve/issues/319) | Observability settings API | `docs/contracts/stv-api-20.md` | BLOCKED by STV-M5-04, STV-M2-14 |
| [STV-API-25 (#299)](https://github.com/djh00t/steve/issues/299) | Normalized error query API | `docs/contracts/stv-api-25.md` | BLOCKED by STV-M5-03, STV-M2-14 |
| [STV-PROV-01 (#380)](https://github.com/djh00t/steve/issues/380) | Required provider inventory and protocol review ownership | `docs/providers/mvp-preset-matrix.md` | READY: inventory proposal only |
| [STV-PROV-02 (#132)](https://github.com/djh00t/steve/issues/132) | Coordinate provider transport contracts | [Provider contract index](../providers/transport-auth-contract.md) | COORDINATION; #491/#492 acceptance and consumer handoff |
| [STV-PROV-38 (#491)](https://github.com/djh00t/steve/issues/491) | TLS backend and trust | `docs/providers/tls-contract.md` | READY: proposal only; runtime consumers blocked |
| [STV-PROV-39 (#492)](https://github.com/djh00t/steve/issues/492) | Account credential to protocol binding | `docs/providers/credential-transport-contract.md` | BLOCKED on accepted #100; no duplicate account schema |
| [STV-PROV-03/04/05/06/07/08/09/10/35 (#383–390, #399)](https://github.com/djh00t/steve/issues/383) | Required OpenAI, Anthropic, Gemini, xAI, DeepSeek, Groq, OpenRouter, generic/local protocol rows | `docs/providers/mvp-preset-matrix.md` | BLOCKED by STV-PROV-01 and STV-PROV-02; one writer serializes edits to this shared file, with row responsibility staying on each producer issue |
| [STV-PROV-13/14 (#381, #382)](https://github.com/djh00t/steve/issues/381) | Subscription authentication discovery | `docs/providers/experimental-auth-decisions.md` | DEFERRED until scope activation |
| [STV-PROV-15/16/17/21 (#391, #392, #393, #397)](https://github.com/djh00t/steve/issues/391) | Experimental provider and gateway discovery | `docs/providers/experimental-provider-matrix.md` | DEFERRED; STV-PROV-21 also depends on STV-M6-ACCEPT-01 |
| [STV-PROV-18/19/20 (#394–396)](https://github.com/djh00t/steve/issues/394) | Bedrock, Azure and Vertex endpoint/identity discovery | `docs/providers/cloud-provider-decisions.md` | DEFERRED until scope activation |
| [STV-M7-01 (#289)](https://github.com/djh00t/steve/issues/289) | TUI toolkit and automation approach | `docs/decisions/STV-M7-01-tui-toolkit.md` | READY: proposal only |
| [STV-M7-02 (#292)](https://github.com/djh00t/steve/issues/292) | Common management-client errors | `docs/decisions/STV-M7-02-management-client.md` | BLOCKED by STV-M7-01, STV-M2-14, STV-M2-17 |
| [STV-M7-29 (#379)](https://github.com/djh00t/steve/issues/379) | Scriptable CLI command and API coverage | `docs/decisions/STV-M7-29-cli-contract.md` | BLOCKED by listed API/domain producers and STV-M7-03 |
| [STV-M8-01 (#335)](https://github.com/djh00t/steve/issues/335) | macOS UI and UI automation platform | `docs/decisions/STV-M8-01-macos-ui-test-platform.md` | READY: proposal only |
| [STV-M8-02 (#336)](https://github.com/djh00t/steve/issues/336) | Native install/update distribution channel | `docs/decisions/STV-M8-02-macos-distribution.md` | READY: proposal only |
| [STV-M8-03 (#337)](https://github.com/djh00t/steve/issues/337) | Deployment discovery and safe-action policy | `docs/decisions/STV-M8-03-deployment-discovery.md` | READY: proposal only |
| [STV-M8-04 (#338)](https://github.com/djh00t/steve/issues/338) | Remote forwarding security | `docs/decisions/STV-M8-04-remote-forward.md` | BLOCKED by STV-M2-14 and STV-M2-17 |
| [STV-M8-05 (#339)](https://github.com/djh00t/steve/issues/339) | macOS credential-storage boundary | `docs/decisions/STV-M8-05-credential-storage.md` | BLOCKED by STV-M2-14 |

## Contract freeze checklist

For applicable wire and data contracts, record: field names and types; required/optional and null/unknown meanings; enum values and error/status mapping; invariants and access/redaction rules; compatibility/version behavior; persistence owner and ordered migration/backfill/rollback behavior; and one fixture plus exact scenario/command. Provider transport contracts also name the protocol/auth boundary; platform decisions state the supported target and qualification command. Do not invent schema values before the producing decision is accepted.

Before a consumer is dispatched, its issue must point to the accepted artifact revision and name the executable contract scenario/command. Semantics live in the accepted contract; persisted shape in versioned migrations; HTTP shape in the API contract plus route tests. Shared migration/route files retain one writer and an explicit integrator.
