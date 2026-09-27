# Reviewed Steve backlog

Snapshot: 2026-09-27; source base `6630577677f40e4ba6a491074f0c91a57d7ddd6e`.

Review tracking: [#60](https://github.com/djh00t/steve/issues/60). GitHub issues contain the complete briefs. This index records the review snapshot; read the linked issue and current merge state before dispatch.

Use the [delivery phases and workstreams](delivery-plan.md), [contract registry](contracts/README.md), [readiness rules](work-packages.md) and [testing policy](testing.md). Estimates are 5–10 active minutes after prerequisites land. BLOCKED leaves require their contract owner to supply exact approved details before dispatch; deferred discovery is not an implementation-ready feature.

## Second-pass decomposition review

The original 385-item inventory was a planning snapshot, not 385 dispatchable junior tasks. The second review at base `a55309d` examined every row for size, executable prerequisites, contract/schema completeness and shared ownership. Each linked area parent records the per-package corrections. Split/combine recommendations preserve existing IDs until the relevant contract producer can publish concrete replacements and update all dependency links.

Of the original 22 READY rows, 21 prepare decisions or qualification proposals; only the process smoke package is implementation work, accepted on main via PR #457. No unresolved money, security, routing or platform choice is approved by a READY label. Integration acceptance gates verify composed results and are not ten-minute feature implementations. The counts below remain the original review snapshot; consult current issues before dispatch.

The immediate test split adds [STV-TST-11 (#458)](https://github.com/djh00t/steve/issues/458) for held object-store writes and [STV-TST-12 (#459)](https://github.com/djh00t/steve/issues/459) for Messages disconnect proof. STV-TST-02 now owns the provider fixture; STV-TST-04 owns Responses only; Chat stays in #33. After adding those two test slices and retiring four duplicate registration-only packages, that review produced 383 active packages; the original summary below remains a 385-row audit snapshot.

## Accepted admission decomposition

The admission contract landed in [#466](https://github.com/djh00t/steve/pull/466) at `63f9cce3d1e754350a1fbb87ee620210520ffa1d`. Its former broad implementation package [#91](https://github.com/djh00t/steve/issues/91) is now a coordination parent for [inference #468](https://github.com/djh00t/steve/issues/468), [management #469](https://github.com/djh00t/steve/issues/469), and [status #470](https://github.com/djh00t/steve/issues/470). These three share `src/server.rs` and execute in that order. The [multi-stream fixture #471](https://github.com/djh00t/steve/issues/471) owns separate test files and can run in parallel. [#92](https://github.com/djh00t/steve/issues/92) remains the composed real-daemon gate after all four leaves land.

Replacing one implementation leaf with three and adding the fixture produced 386 active leaf packages. The independently qualified [process shutdown helper #474](https://github.com/djh00t/steve/issues/474), required before the active-stream drain test #89, brings the inventory to **387 active leaf packages**. The bounded buffering-fault runner [STV-TST-15 #480](https://github.com/djh00t/steve/issues/480), required before mutation CI wiring #79, brings the inventory to **388 active leaf packages**. Coordination parent #91 is not counted as a leaf. The original readiness-count table remains historical; issue bodies hold current ownership and readiness. This decomposition leaves the accepted M0 foundation unchanged.

The next implementation wave must use exact accepted contract revisions and runnable predecessor commands. Register each endpoint with its implementation so its HTTP acceptance can run immediately; serialize shared router, schema, configuration, fixture and CI files. Apply the same rule to clients: qualify an executable target and one working connection before adding views.

## Readiness summary

| Area | READY | BLOCKED | DEFERRED | Total |
| --- | ---: | ---: | ---: | ---: |
| [Testing](https://github.com/djh00t/steve/issues/61) | 1 | 9 | 0 | 10 |
| [Post-M0 reliability](https://github.com/djh00t/steve/issues/62) | 2 | 10 | 0 | 12 |
| [M1](https://github.com/djh00t/steve/issues/37) | 0 | 6 | 0 | 6 |
| [M2](https://github.com/djh00t/steve/issues/63) | 3 | 42 | 0 | 45 |
| [M3](https://github.com/djh00t/steve/issues/64) | 1 | 30 | 0 | 31 |
| [Providers](https://github.com/djh00t/steve/issues/70) | 2 | 24 | 9 | 35 |
| [M4](https://github.com/djh00t/steve/issues/65) | 3 | 41 | 0 | 44 |
| [M5](https://github.com/djh00t/steve/issues/66) | 4 | 51 | 0 | 55 |
| [M6](https://github.com/djh00t/steve/issues/67) | 2 | 41 | 0 | 43 |
| [M7](https://github.com/djh00t/steve/issues/68) | 1 | 33 | 0 | 34 |
| [M8](https://github.com/djh00t/steve/issues/69) | 3 | 26 | 0 | 29 |
| [V1](https://github.com/djh00t/steve/issues/71) | 0 | 0 | 41 | 41 |
| **TOTAL** | **22** | **313** | **50** | **385** |

## Execution order and parallel safety

Start with the real-process harness and independent contract decisions. Complete their concrete outputs and update downstream issue contracts before dispatching consumers. Compute each next wave from merged prerequisites and disjoint write paths; distinct symbols in one file do not imply safe parallel writes. Preserve the active Chat SSE issue [#39](https://github.com/djh00t/steve/issues/39). Existing M1 epics remain open until children and composed acceptance land.

## Scope reconciliation

- **M0 foundation remains accepted (PR #1, 100%).** Post-M0 reliability follow-ups cover journal recovery, backend parity, admission budgets, drain and background failure evidence; they do not reopen the accepted foundation.
- M1 includes missing composed real-process and official SDK proof, cancellation and deferred accounting. Closed implementation history remains closed.
- M2–M6 cover identity, history, money, telemetry and routing, including one composed request tying identity/account/snapshot/session/charge provenance together.
- M7/M8 include management API and runnable client producers before interface consumers. Install/update workflows needing platform decisions are explicit decomposition gates.
- Required provider presets and HTTPS/credential qualification are separated from experimental integrations. Cerebras/Mistral are experimental under the delivery plan; the broader initial-provider wording in the specification must be resolved by provider inventory.
- Post-MVP/v1 items are separately deferred discovery packages. Their output must identify exact implementation leaves and dependencies; no speculative implementation is declared ready.

## Packages

### Testing

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-TST-01](https://github.com/djh00t/steve/issues/72) | Add a real Steve process smoke harness | ACCEPTED (PR #457) | None |
| [STV-TST-02](https://github.com/djh00t/steve/issues/73) | Add a deterministic held-tail provider fixture | BLOCKED | #72 |
| [STV-TST-03](https://github.com/djh00t/steve/issues/74) | Qualify targeted Rust mutation testing | BLOCKED | #72, #73 |
| [STV-TST-04](https://github.com/djh00t/steve/issues/75) | Prove Responses disconnect cancels upstream without replay | BLOCKED | #72, #73 |
| [STV-TST-05](https://github.com/djh00t/steve/issues/76) | Prove official OpenAI SDK streaming compatibility | BLOCKED | #72, #75, #33 |
| [STV-TST-06](https://github.com/djh00t/steve/issues/77) | Prove official Anthropic SDK streaming compatibility | BLOCKED | #72, #459 |
| [STV-TST-07](https://github.com/djh00t/steve/issues/78) | Wire process smoke into PR and main CI | BLOCKED | #72 |
| [STV-TST-08](https://github.com/djh00t/steve/issues/79) | Wire targeted mutations into PR CI | BLOCKED | #74, #78 |
| [STV-TST-09](https://github.com/djh00t/steve/issues/82) | Replace doctor-only backend checks with parity E2E on main | BLOCKED | #80, #81, #78 |
| [STV-TST-10](https://github.com/djh00t/steve/issues/83) | Run official SDK smoke in CI | BLOCKED | #76, #77 |
| [STV-TST-11](https://github.com/djh00t/steve/issues/458) | Add a held S3 PutObject fixture | BLOCKED | #72, #73 |
| [STV-TST-12](https://github.com/djh00t/steve/issues/459) | Prove Messages disconnect cancels upstream without replay | BLOCKED | #72, #73 |

### Post-M0 reliability

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M0-05](https://github.com/djh00t/steve/issues/80) | Prove SQLite and PostgreSQL serve parity | BLOCKED | #72 |
| [STV-M0-06](https://github.com/djh00t/steve/issues/81) | Prove filesystem and S3 object-store parity | BLOCKED | #72 |
| [STV-M0-01](https://github.com/djh00t/steve/issues/84) | Resolve authoritative accounting overflow semantics | READY | None |
| [STV-M0-02](https://github.com/djh00t/steve/issues/85) | Preserve journal framing and report corrupt tails | BLOCKED | #84, #72 |
| [STV-M0-03](https://github.com/djh00t/steve/issues/86) | Retry accounting reconciliation without startup loss | BLOCKED | #84, #72 |
| [STV-M0-04](https://github.com/djh00t/steve/issues/87) | Prove idempotent journal replay after partial DB success | BLOCKED | #84, #72 |
| [STV-M0-07](https://github.com/djh00t/steve/issues/88) | Prove noncritical queue pressure leaves management responsive | BLOCKED | #72, #458 |
| [STV-M0-08](https://github.com/djh00t/steve/issues/89) | Prove live-but-unready drain with an active stream | BLOCKED | #72, #73 |
| [STV-M0-09](https://github.com/djh00t/steve/issues/90) | Define independent admission-budget contract | READY | None |
| [STV-M0-10](https://github.com/djh00t/steve/issues/91) | Coordinate listener admission implementation | COORDINATION | #468, #469, #470 |
| [STV-M0-11](https://github.com/djh00t/steve/issues/92) | Prove listener budgets with a real process | BLOCKED | #91, #72, #73, #471 |
| [STV-M0-12](https://github.com/djh00t/steve/issues/93) | Bound, circuit-break and observe history-worker failures | BLOCKED | #458 |

### M1

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M1-03](https://github.com/djh00t/steve/issues/33) | Verify Chat disconnect cancellation and no replay | BLOCKED | #39, #73 |
| [STV-M1-04](https://github.com/djh00t/steve/issues/94) | Define the Chat accounting event contract | BLOCKED | #84 |
| [STV-M1-05](https://github.com/djh00t/steve/issues/95) | Enqueue completed nonstream Chat attempts | BLOCKED | #94 |
| [STV-M1-06](https://github.com/djh00t/steve/issues/96) | Enqueue streamed Chat attempts at terminal state | BLOCKED | #94, #33 |
| [STV-M1-07](https://github.com/djh00t/steve/issues/97) | Prove saturated Chat accounting does not delay SSE | BLOCKED | #96, #73, #84 |
| [STV-M1-08](https://github.com/djh00t/steve/issues/98) | Run and document the composed M1 acceptance | BLOCKED | #75, #33, #94, #95, #96, #97, #76, #77, #78, #79, #82, #83, #459 |

### M2

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M2-01](https://github.com/djh00t/steve/issues/99) | Freeze local identity and client-auth contract | READY | None |
| [STV-M2-02](https://github.com/djh00t/steve/issues/100) | Freeze provider account, credentials, access and binding contract | BLOCKED | #99 |
| [STV-M2-03](https://github.com/djh00t/steve/issues/101) | Freeze account pool policy, routing and affinity semantics | BLOCKED | #100 |
| [STV-M2-04](https://github.com/djh00t/steve/issues/105) | Add relational organisation, user and client schema | BLOCKED | #99, #103, #104, #72 |
| [STV-M2-14](https://github.com/djh00t/steve/issues/106) | Freeze listener exposure and management authentication contract | READY | None |
| [STV-M2-59](https://github.com/djh00t/steve/issues/107) | Add PostgreSQL organisation, user and client schema | BLOCKED | #105, #104, #102, #72 |
| [STV-M2-05](https://github.com/djh00t/steve/issues/108) | Authenticate local clients and attach resolved identity | BLOCKED | #99, #105, #106, #107, #72 |
| [STV-M2-17](https://github.com/djh00t/steve/issues/109) | Authenticate every management route | BLOCKED | #99, #108, #106, #73, #72 |
| [STV-M2-46](https://github.com/djh00t/steve/issues/110) | Freeze security audit event contract | READY | None |
| [STV-M2-15](https://github.com/djh00t/steve/issues/111) | Add client credential record schema | BLOCKED | #99, #107, #103, #104, #72 |
| [STV-M2-07](https://github.com/djh00t/steve/issues/112) | Add provider and account schema | BLOCKED | #100, #111, #103, #104, #72 |
| [STV-M2-16](https://github.com/djh00t/steve/issues/113) | Add account access-binding schema | BLOCKED | #100, #112, #103, #104, #72 |
| [STV-M2-11](https://github.com/djh00t/steve/issues/114) | Persist per-account health and rate-limit state | BLOCKED | #101, #113, #103, #104, #73, #72 |
| [STV-M2-49](https://github.com/djh00t/steve/issues/115) | Add account-pool schema | BLOCKED | #101, #114, #103, #104, #72 |
| [STV-M2-13](https://github.com/djh00t/steve/issues/118) | Add identity fields to logical request and attempt types | BLOCKED | #99, #105, #108, #107, #72 |
| [STV-M2-47](https://github.com/djh00t/steve/issues/123) | Persist audit events through one shared writer | BLOCKED | #110, #122, #103, #104, #102, #72 |
| [STV-M2-06](https://github.com/djh00t/steve/issues/124) | Manage organisation, user and client records | BLOCKED | #99, #105, #109, #110, #123, #107, #72 |
| [STV-M2-08](https://github.com/djh00t/steve/issues/125) | Manage provider definitions | BLOCKED | #100, #112, #109, #124, #110, #123, #72 |
| [STV-M2-09](https://github.com/djh00t/steve/issues/126) | Enforce account access before provider selection | BLOCKED | #100, #101, #108, #112, #125, #72 |
| [STV-M2-10](https://github.com/djh00t/steve/issues/127) | Select dedicated and prefer-own accounts | BLOCKED | #101, #126, #72 |
| [STV-M2-12](https://github.com/djh00t/steve/issues/128) | Apply user account affinity | BLOCKED | #101, #126, #127, #119, #73, #72 |
| [STV-M2-19](https://github.com/djh00t/steve/issues/129) | Issue one-time client keys | BLOCKED | #99, #105, #111, #109, #124, #110, #123, #72 |
| [STV-M2-20](https://github.com/djh00t/steve/issues/130) | Rotate and revoke client keys | BLOCKED | #99, #108, #111, #129, #110, #123, #72 |
| [STV-M2-41](https://github.com/djh00t/steve/issues/131) | Manage upstream account metadata | BLOCKED | #100, #112, #125, #109, #110, #123, #72 |
| [STV-M2-21](https://github.com/djh00t/steve/issues/136) | Set upstream credentials behind approved secret reference | BLOCKED | #100, #112, #131, #109, #134, #135, #133, #110, #123, #72 |
| [STV-M2-22](https://github.com/djh00t/steve/issues/137) | Manage account access grants | BLOCKED | #100, #113, #109, #125, #110, #123, #136, #72 |
| [STV-M2-23](https://github.com/djh00t/steve/issues/138) | Select weighted accounts | BLOCKED | #101, #126, #127, #73, #72 |
| [STV-M2-58](https://github.com/djh00t/steve/issues/139) | Update per-account health and rate-limit state | BLOCKED | #101, #112, #114, #73, #72 |
| [STV-M2-24](https://github.com/djh00t/steve/issues/140) | Select least-utilised accounts | BLOCKED | #101, #126, #114, #73, #139, #72 |
| [STV-M2-27](https://github.com/djh00t/steve/issues/141) | Fail over to next eligible account | BLOCKED | #98, #101, #126, #127, #114, #73, #139, #72 |
| [STV-M2-28](https://github.com/djh00t/steve/issues/142) | Apply session account affinity | BLOCKED | #101, #126, #127, #119, #73, #72 |
| [STV-M2-42](https://github.com/djh00t/steve/issues/143) | Enforce listener exposure policy | BLOCKED | #106, #73, #109, #72 |
| [STV-M2-50](https://github.com/djh00t/steve/issues/144) | Manage account pools | BLOCKED | #101, #109, #131, #115, #137, #72 |
| [STV-M2-51](https://github.com/djh00t/steve/issues/145) | Attach identity to Responses requests | BLOCKED | #108, #118, #72 |
| [STV-M2-52](https://github.com/djh00t/steve/issues/146) | Attach identity to Chat Completions requests | BLOCKED | #108, #118, #145, #72 |
| [STV-M2-53](https://github.com/djh00t/steve/issues/147) | Attach identity to Anthropic Messages requests | BLOCKED | #108, #118, #146, #72 |
| [STV-M2-55](https://github.com/djh00t/steve/issues/148) | Accept four-user account isolation and routing end to end | BLOCKED | #108, #124, #125, #126, #127, #114, #128, #129, #130, #136, #137, #138, #140, #141, #142, #131, #144, #145, #146, #147, #123, #72 |
| [STV-API-06](https://github.com/djh00t/steve/issues/290) | Freeze account health query contract | BLOCKED | #100, #106 |
| [STV-API-07](https://github.com/djh00t/steve/issues/295) | Read authorized account health | BLOCKED | #290, #114, #131, #109, #72 |
| [STV-API-01](https://github.com/djh00t/steve/issues/296) | Freeze model catalogue management contract | BLOCKED | #106, #187 |
| [STV-API-02](https://github.com/djh00t/steve/issues/304) | Persist managed model catalogue in SQLite | BLOCKED | #296, #103, #72 |
| [STV-API-03](https://github.com/djh00t/steve/issues/305) | Persist managed model catalogue in PostgreSQL | BLOCKED | #296, #104, #72 |
| [STV-API-27](https://github.com/djh00t/steve/issues/309) | Implement model catalogue store | BLOCKED | #296, #304, #305, #72 |
| [STV-API-04](https://github.com/djh00t/steve/issues/312) | Read managed model catalogue | BLOCKED | #296, #304, #305, #109, #72, #309 |
| [STV-API-05](https://github.com/djh00t/steve/issues/361) | Update one model capability record | BLOCKED | #296, #304, #305, #109, #123, #72, #309 |

### M3

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M3-40](https://github.com/djh00t/steve/issues/102) | Freeze migration versioning and backend parity contract | READY | None |
| [STV-M3-13](https://github.com/djh00t/steve/issues/103) | Apply versioned SQLite migrations | BLOCKED | #102, #72 |
| [STV-M3-44](https://github.com/djh00t/steve/issues/104) | Apply PostgreSQL side of migration inventory | BLOCKED | #103, #102, #72 |
| [STV-M3-01](https://github.com/djh00t/steve/issues/116) | Freeze session association and conversation-ID contract | BLOCKED | #99 |
| [STV-M3-03](https://github.com/djh00t/steve/issues/117) | Add session and external-identifier schema | BLOCKED | #115, #116, #103, #104, #72 |
| [STV-M3-04](https://github.com/djh00t/steve/issues/119) | Resolve canonical Steve session IDs and return the session header | BLOCKED | #108, #118, #116, #117, #72 |
| [STV-M3-05](https://github.com/djh00t/steve/issues/120) | Persist logical request-to-session and turn links | BLOCKED | #117, #118, #119, #103, #104, #72 |
| [STV-M3-02](https://github.com/djh00t/steve/issues/121) | Freeze content capture, metadata-only and retention contract | BLOCKED | #116 |
| [STV-M3-06](https://github.com/djh00t/steve/issues/122) | Add searchable message metadata schema | BLOCKED | #120, #121, #103, #104, #72 |
| [STV-M3-07](https://github.com/djh00t/steve/issues/149) | Archive policy-enabled message content | BLOCKED | #121, #120, #122, #123, #72 |
| [STV-M3-08](https://github.com/djh00t/steve/issues/150) | Add content logging and retention configuration | BLOCKED | #121, #110, #123 |
| [STV-M3-09](https://github.com/djh00t/steve/issues/151) | List authorized session metadata | BLOCKED | #109, #118, #120, #122, #125, #119, #144, #72 |
| [STV-M3-10](https://github.com/djh00t/steve/issues/152) | Expire payload objects and references by policy | BLOCKED | #121, #122, #149, #150, #73, #72 |
| [STV-M3-11](https://github.com/djh00t/steve/issues/153) | Exclude credentials from server authentication logs | BLOCKED | #99, #121, #118, #109, #72 |
| [STV-M3-57](https://github.com/djh00t/steve/issues/154) | Write searchable message metadata | BLOCKED | #121, #120, #122, #72 |
| [STV-M3-12](https://github.com/djh00t/steve/issues/155) | Record OpenAI Responses message metadata | BLOCKED | #119, #120, #122, #153, #145, #154, #72 |
| [STV-M3-31](https://github.com/djh00t/steve/issues/156) | Read one authorized session history | BLOCKED | #109, #119, #120, #122, #151, #72 |
| [STV-M3-32](https://github.com/djh00t/steve/issues/157) | Record Chat Completions metadata | BLOCKED | #119, #120, #122, #149, #155, #146, #154, #72 |
| [STV-M3-33](https://github.com/djh00t/steve/issues/158) | Record Anthropic Messages metadata | BLOCKED | #119, #120, #122, #149, #157, #147, #154, #72 |
| [STV-M3-34](https://github.com/djh00t/steve/issues/159) | Exclude Responses prompt content from logs | BLOCKED | #121, #118, #153, #155, #72 |
| [STV-M3-35](https://github.com/djh00t/steve/issues/160) | Exclude Chat Completions prompt content from logs | BLOCKED | #121, #118, #153, #157, #72 |
| [STV-M3-36](https://github.com/djh00t/steve/issues/161) | Exclude Anthropic Messages content from logs | BLOCKED | #121, #118, #153, #158, #72 |
| [STV-M3-43](https://github.com/djh00t/steve/issues/162) | Archive raw payloads by capture policy | BLOCKED | #121, #120, #122, #149, #72 |
| [STV-M3-56](https://github.com/djh00t/steve/issues/163) | Accept cross-provider sessions and content retention end to end | BLOCKED | #119, #120, #122, #149, #150, #151, #152, #155, #156, #157, #158, #162, #103, #104, #102, #154, #72, #73 |
| [STV-API-08](https://github.com/djh00t/steve/issues/318) | Freeze storage and retention management contract | BLOCKED | #121, #106 |
| [STV-API-09](https://github.com/djh00t/steve/issues/322) | Persist managed storage and retention settings in SQLite | BLOCKED | #318, #103, #72 |
| [STV-API-10](https://github.com/djh00t/steve/issues/323) | Persist managed storage and retention settings in PostgreSQL | BLOCKED | #318, #104, #72 |
| [STV-API-28](https://github.com/djh00t/steve/issues/327) | Implement storage and retention settings store | BLOCKED | #318, #322, #323, #72 |
| [STV-API-11](https://github.com/djh00t/steve/issues/330) | Read storage and retention status | BLOCKED | #318, #322, #323, #109, #72, #327 |
| [STV-API-13](https://github.com/djh00t/steve/issues/364) | Update retention policy | BLOCKED | #318, #322, #323, #109, #123, #152, #72, #327 |
| [STV-API-12](https://github.com/djh00t/steve/issues/369) | Update object storage settings | BLOCKED | #318, #322, #323, #109, #123, #72, #327 |

### Providers

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-PROV-02](https://github.com/djh00t/steve/issues/132) | Decide HTTPS transport and credential boundary | READY | None |
| [STV-PROV-37](https://github.com/djh00t/steve/issues/133) | Provide and qualify local TLS upstream fixture | BLOCKED | #132, #72, #73 |
| [STV-PROV-32](https://github.com/djh00t/steve/issues/134) | Wire OpenAI upstream through HTTPS transport | BLOCKED | #132, #133, #100, #72, #73 |
| [STV-PROV-36](https://github.com/djh00t/steve/issues/135) | Wire Anthropic upstream through HTTPS transport | BLOCKED | #132, #133, #100, #72, #73 |
| [STV-PROV-01](https://github.com/djh00t/steve/issues/380) | Inventory required presets and assign protocol review ownership | READY | None |
| [STV-PROV-03](https://github.com/djh00t/steve/issues/383) | Review OpenAI API protocol contract | BLOCKED | #380, #132 |
| [STV-PROV-04](https://github.com/djh00t/steve/issues/384) | Review Gemini protocol contract | BLOCKED | #380, #132 |
| [STV-PROV-05](https://github.com/djh00t/steve/issues/385) | Review xAI/Grok protocol contract | BLOCKED | #380, #132 |
| [STV-PROV-06](https://github.com/djh00t/steve/issues/386) | Review DeepSeek protocol contract | BLOCKED | #380, #132 |
| [STV-PROV-07](https://github.com/djh00t/steve/issues/387) | Review Groq protocol contract | BLOCKED | #380, #132 |
| [STV-PROV-08](https://github.com/djh00t/steve/issues/388) | Review OpenRouter protocol contract | BLOCKED | #380, #132 |
| [STV-PROV-09](https://github.com/djh00t/steve/issues/389) | Review generic and local OpenAI-compatible endpoint contract | BLOCKED | #380, #132 |
| [STV-PROV-10](https://github.com/djh00t/steve/issues/390) | Review generic Anthropic-compatible contract | BLOCKED | #380, #132 |
| [STV-PROV-13](https://github.com/djh00t/steve/issues/381) | Discover GitHub Copilot subscription authentication boundary | DEFERRED | #98 |
| [STV-PROV-14](https://github.com/djh00t/steve/issues/382) | Discover ChatGPT/Codex subscription authentication boundary | DEFERRED | #98 |
| [STV-PROV-15](https://github.com/djh00t/steve/issues/391) | Compare Cerebras and Mistral API preset requirements | DEFERRED | #98 |
| [STV-PROV-16](https://github.com/djh00t/steve/issues/392) | Discover when Ollama needs a dedicated integration | DEFERRED | #98 |
| [STV-PROV-17](https://github.com/djh00t/steve/issues/393) | Compare Vercel AI Gateway, LiteLLM, and Portkey gateway contracts | DEFERRED | #98 |
| [STV-PROV-18](https://github.com/djh00t/steve/issues/394) | Discover AWS Bedrock signing and model contract | DEFERRED | #98 |
| [STV-PROV-19](https://github.com/djh00t/steve/issues/395) | Discover Azure OpenAI and Foundry endpoint contracts | DEFERRED | #98 |
| [STV-PROV-20](https://github.com/djh00t/steve/issues/396) | Discover Google Vertex AI endpoint and identity contract | DEFERRED | #98 |
| [STV-PROV-21](https://github.com/djh00t/steve/issues/397) | Discover Jev decision/evaluation integration boundary | DEFERRED | #291, #98 |
| [STV-PROV-33](https://github.com/djh00t/steve/issues/398) | Assert provider auth headers and credential isolation | BLOCKED | #132, #100, #112, #125, #136, #131, #72, #73 |
| [STV-PROV-22](https://github.com/djh00t/steve/issues/400) | Implement declarative OpenAI API preset | BLOCKED | #383, #100, #112, #125, #136, #131, #134, #398, #72, #73 |
| [STV-PROV-35](https://github.com/djh00t/steve/issues/399) | Review Anthropic API protocol contract | BLOCKED | #380, #132 |
| [STV-PROV-23](https://github.com/djh00t/steve/issues/403) | Implement declarative Anthropic API preset | BLOCKED | #399, #100, #112, #125, #136, #131, #135, #398, #72, #73 |
| [STV-PROV-24](https://github.com/djh00t/steve/issues/401) | Implement declarative Gemini preset | BLOCKED | #384, #100, #112, #125, #136, #131, #134, #398, #72, #73 |
| [STV-PROV-25](https://github.com/djh00t/steve/issues/402) | Implement declarative xAI/Grok API preset | BLOCKED | #385, #100, #112, #125, #136, #131, #134, #398, #72, #73 |
| [STV-PROV-26](https://github.com/djh00t/steve/issues/404) | Implement declarative DeepSeek preset | BLOCKED | #386, #100, #112, #125, #136, #131, #134, #398, #72, #73 |
| [STV-PROV-27](https://github.com/djh00t/steve/issues/405) | Implement declarative Groq preset | BLOCKED | #387, #100, #112, #125, #136, #131, #134, #398, #72, #73 |
| [STV-PROV-28](https://github.com/djh00t/steve/issues/406) | Implement declarative OpenRouter preset | BLOCKED | #388, #100, #112, #125, #136, #131, #134, #398, #72, #73 |
| [STV-PROV-29](https://github.com/djh00t/steve/issues/407) | Implement declarative generic OpenAI-compatible preset | BLOCKED | #389, #100, #112, #125, #136, #131, #134, #398, #72, #73 |
| [STV-PROV-30](https://github.com/djh00t/steve/issues/408) | Implement declarative generic Anthropic-compatible preset | BLOCKED | #390, #100, #112, #125, #136, #131, #135, #398, #72, #73 |
| [STV-PROV-31](https://github.com/djh00t/steve/issues/409) | Implement declarative local OpenAI-compatible endpoint preset | BLOCKED | #389, #100, #112, #125, #136, #131, #134, #398, #72, #73 |
| [STV-PROV-34](https://github.com/djh00t/steve/issues/410) | Add reusable fixture scaffold and preset scenario table | BLOCKED | #380, #72, #73 |

### M4

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M4-01](https://github.com/djh00t/steve/issues/164) | Decide money precision and rounding | READY | None |
| [STV-M4-02](https://github.com/djh00t/steve/issues/165) | Decide FX source and stale-rate policy | READY | None |
| [STV-M4-03](https://github.com/djh00t/steve/issues/166) | Decide usage-price unit and tier semantics | READY | None |
| [STV-M4-04](https://github.com/djh00t/steve/issues/167) | Add SQLite pricing-version table | BLOCKED | #164, #165, #166, #112, #131, #72 |
| [STV-M4-05](https://github.com/djh00t/steve/issues/168) | Add SQLite usage-charge table | BLOCKED | #164, #165, #166, #167, #118, #145, #146, #147, #119, #34, #72 |
| [STV-M4-06](https://github.com/djh00t/steve/issues/169) | Record unknown and non-metered pricing modes | BLOCKED | #166, #167 |
| [STV-M4-07](https://github.com/djh00t/steve/issues/170) | Add shared metered-dimension charge arithmetic | BLOCKED | #164, #166, #167 |
| [STV-M4-12](https://github.com/djh00t/steve/issues/171) | Add PostgreSQL pricing-version table | BLOCKED | #164, #165, #166, #112, #131, #72 |
| [STV-M4-13](https://github.com/djh00t/steve/issues/172) | Add PostgreSQL usage-charge table | BLOCKED | #164, #165, #166, #171, #118, #145, #146, #147, #119, #34, #72 |
| [STV-M4-08](https://github.com/djh00t/steve/issues/173) | Query provider cost rollup | BLOCKED | #168, #172, #170, #112, #131, #72 |
| [STV-M4-14](https://github.com/djh00t/steve/issues/195) | Map output-token usage to shared charge dimension | BLOCKED | #164, #166, #167 |
| [STV-M4-15](https://github.com/djh00t/steve/issues/196) | Capture FX snapshot on charge | BLOCKED | #165, #167, #170, #168, #172 |
| [STV-M4-16](https://github.com/djh00t/steve/issues/197) | Preserve partial billable attempt usage | BLOCKED | #168, #172, #170, #195, #34, #72 |
| [STV-M4-22](https://github.com/djh00t/steve/issues/198) | Select context-tier price | BLOCKED | #166, #167 |
| [STV-M4-17](https://github.com/djh00t/steve/issues/218) | Query account cost rollup | BLOCKED | #168, #172, #170, #118, #145, #146, #147, #119, #72 |
| [STV-M4-18](https://github.com/djh00t/steve/issues/219) | Query user cost rollup | BLOCKED | #168, #172, #170, #118, #145, #146, #147, #119, #72 |
| [STV-M4-19](https://github.com/djh00t/steve/issues/222) | Query client cost rollup | BLOCKED | #168, #172, #170, #118, #145, #146, #147, #119, #72 |
| [STV-M4-20](https://github.com/djh00t/steve/issues/224) | Query session cost rollup | BLOCKED | #168, #172, #170, #118, #145, #146, #147, #119, #72 |
| [STV-M4-21](https://github.com/djh00t/steve/issues/225) | Query model cost rollup | BLOCKED | #168, #172, #170, #118, #145, #146, #147, #119, #72 |
| [STV-M4-23](https://github.com/djh00t/steve/issues/228) | Map cache-read usage to shared charge dimension | BLOCKED | #164, #166, #167 |
| [STV-M4-24](https://github.com/djh00t/steve/issues/229) | Map cache-write usage to shared charge dimension | BLOCKED | #164, #166, #167 |
| [STV-M4-25](https://github.com/djh00t/steve/issues/230) | Map reasoning-token usage to shared charge dimension | BLOCKED | #164, #166, #167 |
| [STV-M4-26](https://github.com/djh00t/steve/issues/231) | Map image usage to shared charge dimension | BLOCKED | #164, #166, #167 |
| [STV-M4-27](https://github.com/djh00t/steve/issues/232) | Map audio usage to shared charge dimension | BLOCKED | #164, #166, #167 |
| [STV-M4-28](https://github.com/djh00t/steve/issues/233) | Map tool usage to shared charge dimension | BLOCKED | #164, #166, #167 |
| [STV-M4-29](https://github.com/djh00t/steve/issues/234) | Apply batch price version | BLOCKED | #166, #167 |
| [STV-M4-30](https://github.com/djh00t/steve/issues/235) | Apply subscription allowance and overage | BLOCKED | #164, #166, #167 |
| [STV-M4-31](https://github.com/djh00t/steve/issues/236) | Record quota consumption mode | BLOCKED | #166, #169 |
| [STV-M4-32](https://github.com/djh00t/steve/issues/237) | Convert known USD charge for local display | BLOCKED | #164, #165, #196 |
| [STV-M4-33](https://github.com/djh00t/steve/issues/238) | Decide display-currency preference contract | BLOCKED | #164, #165 |
| [STV-M4-34](https://github.com/djh00t/steve/issues/241) | Add SQLite display-currency preference | BLOCKED | #238, #118, #145, #146, #147, #72 |
| [STV-M4-35](https://github.com/djh00t/steve/issues/244) | Add PostgreSQL display-currency preference | BLOCKED | #238, #241, #118, #145, #146, #147, #72 |
| [STV-M4-36](https://github.com/djh00t/steve/issues/242) | Serve attributed usage-cost API | BLOCKED | #173, #218, #219, #222, #224, #225, #237, #238, #118, #145, #146, #147, #126, #72 |
| [STV-M4-37](https://github.com/djh00t/steve/issues/239) | Map search usage to shared charge dimension | BLOCKED | #166, #170 |
| [STV-M4-38](https://github.com/djh00t/steve/issues/240) | Map computer-use usage to shared charge dimension | BLOCKED | #166, #170 |
| [STV-M4-39](https://github.com/djh00t/steve/issues/243) | Map video usage to shared charge dimension | BLOCKED | #166, #170 |
| [STV-M4-ACCEPT-01](https://github.com/djh00t/steve/issues/247) | Verify composed accounting acceptance | BLOCKED | #167, #168, #169, #170, #173, #171, #172, #195, #196, #197, #218, #219, #222, #224, #225, #198, #228, #229, #72, #73, #230, #231, #232, #233, #234, #235, #236, #237, #238, #241, #244, #242, #239, #240, #243 |
| [STV-API-14](https://github.com/djh00t/steve/issues/298) | Freeze pricing, FX and usage-report management contract | BLOCKED | #164, #165, #166, #106 |
| [STV-API-15](https://github.com/djh00t/steve/issues/306) | Persist contract-managed FX rates in SQLite | BLOCKED | #298, #103, #72 |
| [STV-API-16](https://github.com/djh00t/steve/issues/307) | Persist contract-managed FX rates in PostgreSQL | BLOCKED | #298, #104, #72 |
| [STV-API-17](https://github.com/djh00t/steve/issues/310) | Read price and FX catalogues | BLOCKED | #298, #167, #171, #306, #307, #109, #72 |
| [STV-API-29](https://github.com/djh00t/steve/issues/362) | Implement FX rate store | BLOCKED | #298, #306, #307, #72 |
| [STV-API-18](https://github.com/djh00t/steve/issues/366) | Write one pricing version | BLOCKED | #298, #167, #171, #109, #123, #72, #362 |
| [STV-API-19](https://github.com/djh00t/steve/issues/367) | Write one FX rate | BLOCKED | #298, #165, #306, #307, #109, #123, #72, #362 |

### M5

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M5-01](https://github.com/djh00t/steve/issues/174) | Decide latency stage and percentile contract | READY | None |
| [STV-M5-02](https://github.com/djh00t/steve/issues/175) | Decide EWMA and reliability rules | READY | None |
| [STV-M5-03](https://github.com/djh00t/steve/issues/176) | Decide normalized error taxonomy | READY | None |
| [STV-M5-04](https://github.com/djh00t/steve/issues/177) | Decide OpenTelemetry MVP export boundary | READY | None |
| [STV-M5-05](https://github.com/djh00t/steve/issues/178) | Add SQLite latency-span table | BLOCKED | #174, #34, #72 |
| [STV-M5-06](https://github.com/djh00t/steve/issues/179) | Measure ingress and request total | BLOCKED | #174, #178, #72 |
| [STV-M5-07](https://github.com/djh00t/steve/issues/180) | Normalize provider 429 errors | BLOCKED | #176, #34 |
| [STV-M5-08](https://github.com/djh00t/steve/issues/181) | Add SQLite error-event table | BLOCKED | #176, #34, #72 |
| [STV-M5-09](https://github.com/djh00t/steve/issues/182) | Calculate p50 and p95 for one sample set | BLOCKED | #174 |
| [STV-M5-10](https://github.com/djh00t/steve/issues/183) | Update latency EWMA for one observation | BLOCKED | #175 |
| [STV-M5-17](https://github.com/djh00t/steve/issues/184) | Measure first-token stage | BLOCKED | #73, #174, #179, #39, #41, #42 |
| [STV-M5-11](https://github.com/djh00t/steve/issues/185) | Export OTEL traces only | BLOCKED | #177, #179, #184, #72 |
| [STV-M5-12](https://github.com/djh00t/steve/issues/199) | Add PostgreSQL latency-span table | BLOCKED | #174, #34, #72 |
| [STV-M5-15](https://github.com/djh00t/steve/issues/200) | Measure session resolution stage | BLOCKED | #174, #179, #119, #188, #72 |
| [STV-M5-16](https://github.com/djh00t/steve/issues/201) | Measure upstream connection stage | BLOCKED | #73, #174, #179, #34 |
| [STV-M5-18](https://github.com/djh00t/steve/issues/202) | Measure accounting persistence stage | BLOCKED | #73, #174, #179, #168 |
| [STV-M5-20](https://github.com/djh00t/steve/issues/203) | Normalize transport errors | BLOCKED | #176 |
| [STV-M5-21](https://github.com/djh00t/steve/issues/204) | Normalize cancellation errors | BLOCKED | #176 |
| [STV-M5-13](https://github.com/djh00t/steve/issues/205) | Add PostgreSQL error-event table | BLOCKED | #176, #34, #72 |
| [STV-M5-27](https://github.com/djh00t/steve/issues/206) | Export structured OTEL logs | BLOCKED | #176, #177, #180, #185, #72 |
| [STV-M5-19](https://github.com/djh00t/steve/issues/210) | Normalize provider 5xx errors | BLOCKED | #176, #34 |
| [STV-M5-25](https://github.com/djh00t/steve/issues/214) | Summarize provider reliability | BLOCKED | #175, #180, #210, #203, #204, #257, #268, #269, #72 |
| [STV-M5-14](https://github.com/djh00t/steve/issues/245) | Measure authentication stage | BLOCKED | #174, #179, #118, #145, #146, #147, #72 |
| [STV-M5-22](https://github.com/djh00t/steve/issues/246) | Query provider latency percentiles | BLOCKED | #178, #182, #72 |
| [STV-M5-23](https://github.com/djh00t/steve/issues/248) | Query account latency percentiles | BLOCKED | #178, #182, #72 |
| [STV-M5-24](https://github.com/djh00t/steve/issues/249) | Query model latency percentiles | BLOCKED | #178, #182, #72 |
| [STV-M5-28](https://github.com/djh00t/steve/issues/250) | Query account reliability | BLOCKED | #214, #175, #72 |
| [STV-M5-29](https://github.com/djh00t/steve/issues/251) | Query model reliability | BLOCKED | #214, #175, #72 |
| [STV-M5-30](https://github.com/djh00t/steve/issues/252) | Measure first-byte timing | BLOCKED | #174, #179, #39, #41, #42, #73 |
| [STV-M5-31](https://github.com/djh00t/steve/issues/253) | Measure request transformation stage | BLOCKED | #174, #179, #40, #44, #72 |
| [STV-M5-32](https://github.com/djh00t/steve/issues/254) | Measure response transformation stage | BLOCKED | #174, #179, #40, #44, #72 |
| [STV-M5-33](https://github.com/djh00t/steve/issues/255) | Query one request timeline | BLOCKED | #178, #181, #199, #205, #72 |
| [STV-M5-26](https://github.com/djh00t/steve/issues/256) | Export privacy-safe OTEL metrics | BLOCKED | #177, #185, #170, #72 |
| [STV-M5-34](https://github.com/djh00t/steve/issues/257) | Normalize protocol and malformed-response errors | BLOCKED | #176, #34 |
| [STV-M5-35](https://github.com/djh00t/steve/issues/258) | Query provider latency EWMAs | BLOCKED | #175, #183, #178, #72 |
| [STV-M5-36](https://github.com/djh00t/steve/issues/259) | Query account latency EWMAs | BLOCKED | #175, #183, #178, #72 |
| [STV-M5-37](https://github.com/djh00t/steve/issues/260) | Query model latency EWMAs | BLOCKED | #175, #183, #178, #72 |
| [STV-M5-38](https://github.com/djh00t/steve/issues/261) | Expose request timeline management endpoint | BLOCKED | #255, #118, #145, #146, #147, #126, #72 |
| [STV-M5-39](https://github.com/djh00t/steve/issues/262) | Expose provider/account/model telemetry summary API | BLOCKED | #246, #248, #249, #214, #250, #251, #258, #259, #260, #126, #72 |
| [STV-M5-41](https://github.com/djh00t/steve/issues/263) | Measure identity resolution stage | BLOCKED | #174, #179, #118, #145, #146, #147, #72 |
| [STV-M5-42](https://github.com/djh00t/steve/issues/264) | Measure model-routing stage | BLOCKED | #174, #179, #188, #72 |
| [STV-M5-43](https://github.com/djh00t/steve/issues/265) | Measure account-selection stage | BLOCKED | #174, #179, #190, #72 |
| [STV-M5-44](https://github.com/djh00t/steve/issues/266) | Measure response-header stage | BLOCKED | #174, #201, #34, #73 |
| [STV-M5-45](https://github.com/djh00t/steve/issues/267) | Measure final-token stage | BLOCKED | #174, #184, #39, #41, #42, #73 |
| [STV-M5-46](https://github.com/djh00t/steve/issues/268) | Normalize timeout errors | BLOCKED | #176 |
| [STV-M5-47](https://github.com/djh00t/steve/issues/269) | Normalize local overload errors | BLOCKED | #176 |
| [STV-M5-ACCEPT-01](https://github.com/djh00t/steve/issues/271) | Verify composed telemetry acceptance | BLOCKED | #178, #179, #180, #181, #182, #183, #185, #199, #205, #245, #200, #201, #184, #202, #210, #203, #204, #246, #248, #249, #214, #256, #206, #250, #251, #252, #253, #254, #255, #72, #73, #257, #258, #259, #260, #261, #262, #263, #264, #265, #266, #267, #268, #269 |
| [STV-API-25](https://github.com/djh00t/steve/issues/299) | Freeze normalized error query contract | BLOCKED | #176, #106 |
| [STV-API-26](https://github.com/djh00t/steve/issues/317) | Read paged normalized request errors | BLOCKED | #299, #176, #181, #205, #109, #72 |
| [STV-API-20](https://github.com/djh00t/steve/issues/319) | Freeze observability settings management contract | BLOCKED | #177, #106 |
| [STV-API-21](https://github.com/djh00t/steve/issues/324) | Persist managed observability settings in SQLite | BLOCKED | #319, #103, #72 |
| [STV-API-22](https://github.com/djh00t/steve/issues/325) | Persist managed observability settings in PostgreSQL | BLOCKED | #319, #104, #72 |
| [STV-API-30](https://github.com/djh00t/steve/issues/328) | Implement observability settings store | BLOCKED | #319, #324, #325, #72 |
| [STV-API-23](https://github.com/djh00t/steve/issues/331) | Read observability settings | BLOCKED | #319, #324, #325, #109, #72, #328 |
| [STV-API-24](https://github.com/djh00t/steve/issues/370) | Update observability settings | BLOCKED | #319, #324, #325, #109, #123, #72, #328 |

### M6

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M6-01](https://github.com/djh00t/steve/issues/186) | Decide deterministic routing score contract | READY | None |
| [STV-M6-02](https://github.com/djh00t/steve/issues/187) | Decide fixed/rule routing precedence | READY | None |
| [STV-M6-03](https://github.com/djh00t/steve/issues/188) | Pin RoutingEngine boundary for Switchyard | BLOCKED | #186, #187 |
| [STV-M6-04](https://github.com/djh00t/steve/issues/189) | Add SQLite routing-profile tables | BLOCKED | #187, #72 |
| [STV-M6-05](https://github.com/djh00t/steve/issues/190) | Filter accounts by user/client access | BLOCKED | #126, #112, #131, #186, #188, #72 |
| [STV-M6-06](https://github.com/djh00t/steve/issues/191) | Score candidates by known cost | BLOCKED | #186, #170 |
| [STV-M6-07](https://github.com/djh00t/steve/issues/192) | Add SQLite routing-decision table | BLOCKED | #186, #191, #72 |
| [STV-M6-08](https://github.com/djh00t/steve/issues/193) | Retry one retryable failure before stream commit | BLOCKED | #190, #191, #39, #41, #42, #176, #73 |
| [STV-M6-09](https://github.com/djh00t/steve/issues/194) | Resolve model/profile into routing request | BLOCKED | #187, #188 |
| [STV-M6-10](https://github.com/djh00t/steve/issues/207) | Add PostgreSQL routing-profile tables | BLOCKED | #187, #72 |
| [STV-M6-11](https://github.com/djh00t/steve/issues/208) | Filter account model capability | BLOCKED | #186, #112, #131 |
| [STV-M6-12](https://github.com/djh00t/steve/issues/211) | Filter accounts by context capacity | BLOCKED | #186, #208, #112, #131 |
| [STV-M6-13](https://github.com/djh00t/steve/issues/209) | Filter accounts by pool eligibility | BLOCKED | #115, #144, #186 |
| [STV-M6-15](https://github.com/djh00t/steve/issues/217) | Score candidates by reliability | BLOCKED | #186, #214 |
| [STV-M6-16](https://github.com/djh00t/steve/issues/212) | Score candidates by quota availability | BLOCKED | #186, #112, #131 |
| [STV-M6-17](https://github.com/djh00t/steve/issues/213) | Score candidate affinity | BLOCKED | #186, #115, #144, #119 |
| [STV-M6-14](https://github.com/djh00t/steve/issues/215) | Score candidates by measured latency | BLOCKED | #186, #182 |
| [STV-M6-18](https://github.com/djh00t/steve/issues/220) | Combine approved score components | BLOCKED | #186, #191, #215, #217, #212, #213 |
| [STV-M6-19](https://github.com/djh00t/steve/issues/216) | Add PostgreSQL routing-decision table | BLOCKED | #186, #191, #72 |
| [STV-M6-20](https://github.com/djh00t/steve/issues/221) | Persist one routing decision | BLOCKED | #186, #191, #192, #216, #72 |
| [STV-M6-22](https://github.com/djh00t/steve/issues/223) | Dispatch OpenAI Chat via selected account | BLOCKED | #190, #220, #221, #40, #44, #72 |
| [STV-M6-23](https://github.com/djh00t/steve/issues/226) | Dispatch OpenAI Responses through selected account | BLOCKED | #223, #40, #44, #72 |
| [STV-M6-24](https://github.com/djh00t/steve/issues/227) | Dispatch Anthropic Messages through selected account | BLOCKED | #223, #40, #44, #72 |
| [STV-M6-26](https://github.com/djh00t/steve/issues/270) | Decide immutable routing-snapshot contract | BLOCKED | #167, #174, #126, #112, #131, #187 |
| [STV-M6-27](https://github.com/djh00t/steve/issues/274) | Build eligibility/model snapshot fragment | BLOCKED | #112, #131, #270, #126 |
| [STV-M6-28](https://github.com/djh00t/steve/issues/275) | Build price/FX snapshot fragment | BLOCKED | #167, #270 |
| [STV-M6-29](https://github.com/djh00t/steve/issues/276) | Build quota/rate-limit snapshot fragment | BLOCKED | #112, #131, #270 |
| [STV-M6-30](https://github.com/djh00t/steve/issues/277) | Build latency/reliability snapshot fragment | BLOCKED | #246, #270, #248, #249, #214, #250, #251, #258, #259, #260 |
| [STV-M6-31](https://github.com/djh00t/steve/issues/278) | Build profile/alias/rule snapshot fragment | BLOCKED | #189, #270 |
| [STV-M6-32](https://github.com/djh00t/steve/issues/282) | Publish immutable routing snapshot atomically | BLOCKED | #270, #274, #275, #276, #277, #278 |
| [STV-M6-33](https://github.com/djh00t/steve/issues/285) | Read one immutable routing snapshot per request | BLOCKED | #282, #270 |
| [STV-M6-34](https://github.com/djh00t/steve/issues/288) | Record routing snapshot version on decision | BLOCKED | #221, #270, #282, #285, #72 |
| [STV-M6-35](https://github.com/djh00t/steve/issues/272) | Decide Switchyard adapter and dependency boundary | BLOCKED | #188 |
| [STV-M6-36](https://github.com/djh00t/steve/issues/279) | Implement Switchyard RoutingEngine adapter | BLOCKED | #188, #272 |
| [STV-M6-42](https://github.com/djh00t/steve/issues/273) | Decide profile/alias/rule management API contract | BLOCKED | #187, #189 |
| [STV-M6-37](https://github.com/djh00t/steve/issues/280) | Read routing profile through management API | BLOCKED | #189, #207, #187, #273, #72 |
| [STV-M6-38](https://github.com/djh00t/steve/issues/281) | Write routing profile through management API | BLOCKED | #189, #207, #187, #273, #72 |
| [STV-M6-39](https://github.com/djh00t/steve/issues/283) | Resolve model alias through management API | BLOCKED | #189, #207, #187, #273, #72 |
| [STV-M6-40](https://github.com/djh00t/steve/issues/284) | Read ordered routing rules through management API | BLOCKED | #189, #207, #187, #273, #72 |
| [STV-M6-41](https://github.com/djh00t/steve/issues/286) | Write ordered routing rule through management API | BLOCKED | #189, #207, #187, #273, #72 |
| [STV-M6-21](https://github.com/djh00t/steve/issues/287) | Stop failover after stream commitment | BLOCKED | #193, #39, #41, #42, #73 |
| [STV-M6-ACCEPT-01](https://github.com/djh00t/steve/issues/291) | Verify composed routing acceptance | BLOCKED | #186, #187, #188, #189, #190, #191, #192, #193, #194, #207, #208, #211, #209, #215, #217, #212, #213, #220, #216, #221, #287, #223, #226, #227, #270, #282, #285, #288, #279, #72, #73, #272, #280, #281, #283, #284, #286, #273 |
| [STV-MVP-ACCEPT-01](https://github.com/djh00t/steve/issues/294) | Verify authenticated request evidence chain | BLOCKED | #148, #163, #247, #291, #72, #73, #459 |

### M7

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M7-01](https://github.com/djh00t/steve/issues/289) | Freeze TUI toolkit and automation approach | READY | None |
| [STV-M7-02](https://github.com/djh00t/steve/issues/292) | Freeze common management client errors | BLOCKED | #289, #106, #109 |
| [STV-M7-03](https://github.com/djh00t/steve/issues/293) | Create TUI navigation shell | BLOCKED | #289 |
| [STV-M7-30](https://github.com/djh00t/steve/issues/297) | Implement authenticated management GET client | BLOCKED | #289, #292, #109, #72, #293 |
| [STV-M7-04](https://github.com/djh00t/steve/issues/300) | Render dashboard status fields | BLOCKED | #292, #293, #297 |
| [STV-M7-05](https://github.com/djh00t/steve/issues/301) | Render provider health states | BLOCKED | #292, #293, #125, #297 |
| [STV-M7-06](https://github.com/djh00t/steve/issues/302) | Render safe account rows | BLOCKED | #292, #293, #131, #295, #297 |
| [STV-M7-07](https://github.com/djh00t/steve/issues/303) | Render user-to-client attribution | BLOCKED | #292, #293, #124, #297 |
| [STV-M7-08](https://github.com/djh00t/steve/issues/315) | Render model capability states | BLOCKED | #292, #293, #312, #297 |
| [STV-M7-09](https://github.com/djh00t/steve/issues/313) | Render pricing and FX states | BLOCKED | #292, #293, #310, #297 |
| [STV-M7-10](https://github.com/djh00t/steve/issues/308) | Render active profiles and ordered routes | BLOCKED | #292, #293, #280, #281, #283, #284, #286, #297 |
| [STV-M7-11](https://github.com/djh00t/steve/issues/311) | Render session history availability | BLOCKED | #292, #293, #151, #156, #297 |
| [STV-M7-12](https://github.com/djh00t/steve/issues/314) | Render daily and monthly cost states | BLOCKED | #292, #293, #242, #297 |
| [STV-M7-13](https://github.com/djh00t/steve/issues/316) | Render latency sample states | BLOCKED | #292, #293, #262, #297 |
| [STV-M7-14](https://github.com/djh00t/steve/issues/321) | Render redacted request errors | BLOCKED | #292, #293, #317, #297 |
| [STV-M7-15](https://github.com/djh00t/steve/issues/333) | Render storage and retention state | BLOCKED | #292, #293, #330, #297 |
| [STV-M7-16](https://github.com/djh00t/steve/issues/334) | Render observability settings safely | BLOCKED | #292, #293, #331, #297 |
| [STV-M7-17](https://github.com/djh00t/steve/issues/320) | Render daemon diagnostics states | BLOCKED | #292, #293, #297 |
| [STV-M7-18](https://github.com/djh00t/steve/issues/326) | Support authenticated remote TUI targets | BLOCKED | #292, #106, #109, #297, #293 |
| [STV-M7-19](https://github.com/djh00t/steve/issues/329) | Exercise TUI dashboard and one config change through daemon/API/UI | BLOCKED | #300, #308, #72, #280, #281, #297, #293 |
| [STV-M7-20](https://github.com/djh00t/steve/issues/332) | Verify TUI route inventory against API coverage | BLOCKED | #292, #293 |
| [STV-M7-21](https://github.com/djh00t/steve/issues/358) | Edit provider/account metadata | BLOCKED | #292, #293, #125, #131, #136, #297 |
| [STV-M7-22](https://github.com/djh00t/steve/issues/360) | Edit users and client access | BLOCKED | #292, #293, #124, #129, #130, #297 |
| [STV-M7-23](https://github.com/djh00t/steve/issues/365) | Edit model capabilities | BLOCKED | #292, #293, #361, #297 |
| [STV-M7-24](https://github.com/djh00t/steve/issues/371) | Edit prices and FX rates | BLOCKED | #292, #293, #366, #367, #297 |
| [STV-M7-25](https://github.com/djh00t/steve/issues/363) | Edit route rules | BLOCKED | #292, #293, #286, #297 |
| [STV-M7-26](https://github.com/djh00t/steve/issues/368) | Edit session/history retention policy | BLOCKED | #292, #293, #364, #297 |
| [STV-M7-27](https://github.com/djh00t/steve/issues/374) | Edit storage and observability settings | BLOCKED | #292, #293, #369, #370, #297 |
| [STV-M7-28](https://github.com/djh00t/steve/issues/375) | Compose M7 configure/report acceptance | BLOCKED | #329, #332, #358, #371, #363, #72, #293 |
| [STV-M7-29](https://github.com/djh00t/steve/issues/379) | Freeze scriptable CLI command and API coverage | BLOCKED | #292, #99, #100, #106, #151, #156, #242, #280, #281, #283, #284, #286, #102, #293 |

### M8

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-M8-01](https://github.com/djh00t/steve/issues/335) | Freeze macOS UI and UI automation platform | READY | None |
| [STV-M8-02](https://github.com/djh00t/steve/issues/336) | Freeze native install/update channel | READY | None |
| [STV-M8-03](https://github.com/djh00t/steve/issues/337) | Freeze deployment discovery and safe action policy | READY | None |
| [STV-M8-04](https://github.com/djh00t/steve/issues/338) | Freeze remote forwarding security contract | BLOCKED | #106, #109 |
| [STV-M8-05](https://github.com/djh00t/steve/issues/339) | Freeze macOS credential storage boundary | BLOCKED | #106 |
| [STV-M8-29](https://github.com/djh00t/steve/issues/340) | Create macOS menu-bar target with disconnected state | BLOCKED | #335, #72 |
| [STV-M8-06](https://github.com/djh00t/steve/issues/342) | Define deployment connection model | BLOCKED | #335, #337, #340 |
| [STV-M8-07](https://github.com/djh00t/steve/issues/343) | Probe configured remote deployment | BLOCKED | #336, #339, #106, #109, #340 |
| [STV-M8-08](https://github.com/djh00t/steve/issues/344) | Probe local management endpoint | BLOCKED | #335, #337, #340 |
| [STV-M8-09](https://github.com/djh00t/steve/issues/345) | Detect native Steve service | BLOCKED | #336, #337, #340 |
| [STV-M8-10](https://github.com/djh00t/steve/issues/346) | Detect supported local container runtimes | BLOCKED | #337, #340 |
| [STV-M8-11](https://github.com/djh00t/steve/issues/348) | Present onboarding choices | BLOCKED | #342, #343, #344, #345, #346, #340 |
| [STV-M8-12](https://github.com/djh00t/steve/issues/347) | Connect to a remote Steve deployment | BLOCKED | #339, #343, #106, #109, #340 |
| [STV-M8-13](https://github.com/djh00t/steve/issues/341) | Decompose native installation | BLOCKED | #336, #337, #293 |
| [STV-M8-14](https://github.com/djh00t/steve/issues/349) | Start native Steve from the controller | BLOCKED | #337, #89, #340 |
| [STV-M8-15](https://github.com/djh00t/steve/issues/350) | Decompose native update | BLOCKED | #336, #337, #89, #293 |
| [STV-M8-16](https://github.com/djh00t/steve/issues/351) | Decompose local container setup | BLOCKED | #337, #1, #89, #293 |
| [STV-M8-17](https://github.com/djh00t/steve/issues/352) | Connect optional loopback forwarding | BLOCKED | #338, #339, #106, #109, #340 |
| [STV-M8-18](https://github.com/djh00t/steve/issues/353) | Show provider health and usage | BLOCKED | #335, #347, #125, #131, #295, #242, #340 |
| [STV-M8-19](https://github.com/djh00t/steve/issues/354) | Show daily and monthly spend | BLOCKED | #335, #242, #340 |
| [STV-M8-20](https://github.com/djh00t/steve/issues/355) | Switch active routing profile | BLOCKED | #335, #106, #280, #281, #340 |
| [STV-M8-21](https://github.com/djh00t/steve/issues/356) | Switch active deployment | BLOCKED | #342, #343, #344, #345, #346, #340 |
| [STV-M8-22](https://github.com/djh00t/steve/issues/357) | Open full management TUI | BLOCKED | #289, #297, #335, #340 |
| [STV-M8-23](https://github.com/djh00t/steve/issues/359) | Exercise macOS onboarding through control plane | BLOCKED | #348, #347, #353, #354, #355, #109, #242, #280, #281, #340 |
| [STV-M8-24](https://github.com/djh00t/steve/issues/372) | Verify discovery and safe onboarding coverage | BLOCKED | #342, #343, #344, #345, #346, #348, #340 |
| [STV-M8-25](https://github.com/djh00t/steve/issues/373) | Verify deployment switch failure preserves active target | BLOCKED | #356, #340 |
| [STV-M8-26](https://github.com/djh00t/steve/issues/376) | Stop native Steve through graceful drain | BLOCKED | #337, #89, #340 |
| [STV-M8-27](https://github.com/djh00t/steve/issues/377) | Decompose container update and shutdown | BLOCKED | #337, #1, #89, #293 |
| [STV-M8-28](https://github.com/djh00t/steve/issues/378) | Edit one basic routing rule | BLOCKED | #335, #106, #286, #340 |

### V1

| Package | Outcome | Readiness | Prerequisites |
| --- | --- | --- | --- |
| [STV-V1-01](https://github.com/djh00t/steve/issues/411) | Complete browser administration | DEFERRED | #375, #359, #372, #373 |
| [STV-V1-02](https://github.com/djh00t/steve/issues/412) | Explore sessions and history | DEFERRED | #163, #375, #359, #372, #373 |
| [STV-V1-03](https://github.com/djh00t/steve/issues/413) | Explore cost and latency dashboards | DEFERRED | #247, #271, #375 |
| [STV-V1-04](https://github.com/djh00t/steve/issues/414) | Explore routing-decision history | DEFERRED | #291, #375 |
| [STV-V1-05](https://github.com/djh00t/steve/issues/415) | Discover OIDC and SSO requirements | DEFERRED | #148, #359, #372, #373 |
| [STV-V1-06](https://github.com/djh00t/steve/issues/416) | Discover role-based access control requirements | DEFERRED | #148 |
| [STV-V1-07](https://github.com/djh00t/steve/issues/417) | Discover team and group identity requirements | DEFERRED | #148 |
| [STV-V1-08](https://github.com/djh00t/steve/issues/418) | Discover per-team provider policy requirements | DEFERRED | #148, #291 |
| [STV-V1-09](https://github.com/djh00t/steve/issues/419) | Discover budget and spend-limit requirements | DEFERRED | #247, #291 |
| [STV-V1-10](https://github.com/djh00t/steve/issues/420) | Discover cost-centre and project-tag requirements | DEFERRED | #163, #247 |
| [STV-V1-11](https://github.com/djh00t/steve/issues/421) | Discover audit administration requirements | DEFERRED | #148, #163 |
| [STV-V1-12](https://github.com/djh00t/steve/issues/422) | Discover native supervisor/coordinator requirements | DEFERRED | #1, #359, #372, #373 |
| [STV-V1-13](https://github.com/djh00t/steve/issues/423) | Discover signed release and versioned installation requirements | DEFERRED | #359, #372, #373 |
| [STV-V1-14](https://github.com/djh00t/steve/issues/424) | Discover upgrade cutover and rollback requirements | DEFERRED | #1, #359, #372, #373 |
| [STV-V1-15](https://github.com/djh00t/steve/issues/426) | Discover manual, scheduled and automatic upgrade policy | DEFERRED | #359, #372, #373, #423 |
| [STV-V1-16](https://github.com/djh00t/steve/issues/428) | Discover expand/contract database migration requirements | DEFERRED | #1, #359, #372, #373, #424 |
| [STV-V1-17](https://github.com/djh00t/steve/issues/425) | Discover stateless gateway replica requirements | DEFERRED | #1, #148, #163, #359, #372, #373 |
| [STV-V1-18](https://github.com/djh00t/steve/issues/427) | Discover PostgreSQL HA operating requirements | DEFERRED | #1, #359, #372, #373 |
| [STV-V1-19](https://github.com/djh00t/steve/issues/429) | Set evidence threshold for Redis | DEFERRED | #1, #359, #372, #373, #425 |
| [STV-V1-20](https://github.com/djh00t/steve/issues/430) | Discover Kubernetes and Helm support requirements | DEFERRED | #1, #359, #372, #373 |
| [STV-V1-21](https://github.com/djh00t/steve/issues/431) | Discover rolling-upgrade requirements | DEFERRED | #1, #359, #372, #373, #424, #428 |
| [STV-V1-22](https://github.com/djh00t/steve/issues/432) | Discover distributed rate and quota state requirements | DEFERRED | #148, #247, #359, #372, #373 |
| [STV-V1-23](https://github.com/djh00t/steve/issues/433) | Discover enterprise secrets backend requirements | DEFERRED | #148, #359, #372, #373 |
| [STV-V1-24](https://github.com/djh00t/steve/issues/434) | Discover mutual TLS requirements | DEFERRED | #148, #359, #372, #373 |
| [STV-V1-25](https://github.com/djh00t/steve/issues/435) | Discover tenant encryption and retention requirements | DEFERRED | #163, #359, #372, #373 |
| [STV-V1-26](https://github.com/djh00t/steve/issues/436) | Discover object-store lifecycle requirements | DEFERRED | #163, #359, #372, #373 |
| [STV-V1-27](https://github.com/djh00t/steve/issues/437) | Discover legal hold, export and delete requirements | DEFERRED | #163, #359, #372, #373 |
| [STV-V1-28](https://github.com/djh00t/steve/issues/438) | Discover richer Switchyard strategy requirements | DEFERRED | #291 |
| [STV-V1-29](https://github.com/djh00t/steve/issues/439) | Discover Jev and specialist classifier requirements | DEFERRED | #291 |
| [STV-V1-30](https://github.com/djh00t/steve/issues/440) | Discover A/B routing experiment requirements | DEFERRED | #247, #271, #291 |
| [STV-V1-31](https://github.com/djh00t/steve/issues/441) | Discover learned routing policy requirements | DEFERRED | #247, #271, #291 |
| [STV-V1-32](https://github.com/djh00t/steve/issues/442) | Discover geographic routing requirements | DEFERRED | #291 |
| [STV-V1-33](https://github.com/djh00t/steve/issues/443) | Discover subscription and quota marginal-cost requirements | DEFERRED | #247, #291 |
| [STV-V1-34](https://github.com/djh00t/steve/issues/444) | Discover cross-provider conversation rehydration requirements | DEFERRED | #163, #291 |
| [STV-V1-35](https://github.com/djh00t/steve/issues/445) | Discover thread import/export requirements | DEFERRED | #163 |
| [STV-V1-36](https://github.com/djh00t/steve/issues/446) | Discover session summary and compaction requirements | DEFERRED | #163 |
| [STV-V1-37](https://github.com/djh00t/steve/issues/447) | Discover provider continuation-token requirements | DEFERRED | #163, #291 |
| [STV-V1-38](https://github.com/djh00t/steve/issues/448) | Discover official Grafana dashboard requirements | DEFERRED | #271 |
| [STV-V1-39](https://github.com/djh00t/steve/issues/449) | Discover OTEL Collector and Grafana Alloy examples | DEFERRED | #271 |
| [STV-V1-40](https://github.com/djh00t/steve/issues/450) | Discover metrics/logs/traces backend examples | DEFERRED | #271 |
| [STV-V1-41](https://github.com/djh00t/steve/issues/451) | Discover SLO and alert requirements | DEFERRED | #271 |

## Retired registration-only packages

These packages are superseded, not delivered features. Route registration now belongs to each endpoint implementation below, including its real-daemon HTTP acceptance. Each endpoint exclusively owns its registration edit in `src/server.rs::management_router`; these shared-file edits are serialized. The API-31 → API-32 → API-33 → API-34 dependency chain is removed; endpoint prerequisites remain their contracts, stores, authentication and test producers. No active consumer depends on a retired package.

| Retired package | Endpoint packages absorbing route registration |
| --- | --- |
| [STV-API-31 (#452)](https://github.com/djh00t/steve/issues/452) | [STV-M2-06 (#124)](https://github.com/djh00t/steve/issues/124), [STV-M2-08 (#125)](https://github.com/djh00t/steve/issues/125), [STV-M2-41 (#131)](https://github.com/djh00t/steve/issues/131), [STV-API-04 (#312)](https://github.com/djh00t/steve/issues/312), [STV-API-05 (#361)](https://github.com/djh00t/steve/issues/361), [STV-API-07 (#295)](https://github.com/djh00t/steve/issues/295) |
| [STV-API-32 (#453)](https://github.com/djh00t/steve/issues/453) | [STV-M3-09 (#151)](https://github.com/djh00t/steve/issues/151), [STV-M3-31 (#156)](https://github.com/djh00t/steve/issues/156), [STV-API-11 (#330)](https://github.com/djh00t/steve/issues/330), [STV-API-12 (#369)](https://github.com/djh00t/steve/issues/369), [STV-API-13 (#364)](https://github.com/djh00t/steve/issues/364) |
| [STV-API-33 (#454)](https://github.com/djh00t/steve/issues/454) | [STV-API-17 (#310)](https://github.com/djh00t/steve/issues/310), [STV-API-18 (#366)](https://github.com/djh00t/steve/issues/366), [STV-API-19 (#367)](https://github.com/djh00t/steve/issues/367) |
| [STV-API-34 (#455)](https://github.com/djh00t/steve/issues/455) | [STV-M5-39 (#262)](https://github.com/djh00t/steve/issues/262), [STV-API-23 (#331)](https://github.com/djh00t/steve/issues/331), [STV-API-24 (#370)](https://github.com/djh00t/steve/issues/370), [STV-API-26 (#317)](https://github.com/djh00t/steve/issues/317) |
