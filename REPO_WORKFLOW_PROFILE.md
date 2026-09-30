# Steve workflow profile

Local binding of [PORTABLE_WORKFLOW.md](PORTABLE_WORKFLOW.md), version 1.
The portable controller, contract, skill, and templates were copied from
`djh00t/agent-brain` at `be0a0e1776985ab8119b169692e759fb4b6bb846` on
2026-09-30. The executable binding is
[steve_delivery_reset.json](agent_workflow/profiles/steve_delivery_reset.json).
Recheck the tracker, release head, ownership bridge, open pull requests, CI, and
native model availability before every claim.

| Field | Steve binding and inspected source |
| --- | --- |
| Adoption | Version 1 from pinned agent-brain commit `be0a0e1776985ab8119b169692e759fb4b6bb846`; local bootstrap owner `/root/provider_carryforward`; no controller customization |
| Identity and ingress | `github.com/djh00t/steve`; current Codex task is the primary ingress and `/root` is release orchestrator |
| Delivery sources | GitHub Issues are canonical tickets; `docs/backlog-index.md`, `docs/delivery-plan.md`, and `docs/work-packages.json` are repository planning and inventory projections. New `WF-` issues carry one `workflow-json` block and use tracker acceptance. The external pre-adoption ownership bridge contains only sourced active assignments; SQLite owns new claims and receipts |
| Contracts | `docs/contracts/README.md`, accepted contract files, and the exact issue revisions referenced by a package; missing accepted revisions remain blockers |
| Release | GitHub default branch `main`; integration branch `codex/delivery-reset`, pushed at `e389c86b801c2ea7a5acd96f3d45e917c5175f7c` on 2026-09-30. Child branches use `codex/<package>-<description>`. Current authority permits `/root` to integrate reviewed local commits into this branch; it does not grant a GitHub merge or formal approval. David Hooton owns the release merge to `main` |
| Checks | From repository root: `python3 -m agent_workflow.demo` for the copied controller; `python3 scripts/test_dispatch_work_packages.py` for the inventory; focused package tests plus `make check` for local changed scope. `make quality-gates` belongs to post-merge main CI and is not a local or pre-PR command. Package check IDs select only relevant reviewed commands |
| Evidence | GitHub Actions `.github/workflows/ci.yml` supplies PR `quality`, `official-sdk`, macOS, Windows, integrations, and path-conditional mutation evidence; `container` runs on pushes. Current branch-protection requirements are verified separately. Bind evidence to exact head/base and actual run URLs. M1 release evidence additionally retains its conditional mutation and real Windows/NTFS requirements. Local runs and inventory output are separate evidence categories |
| Runtime boundaries | Preserve active workers and dirty files. Protected resources are the adoption bootstrap/review, dispatch inventory, M1 accounting, and shared release integration. Coordination is under `~/.agents/workflow/steve/delivery-reset`; its pre-adoption bridge is ownership only, not a backlog or acceptance ledger. Remove rehearsal directories; retain live claims/receipts for takeover |
| Authority | The current task authorizes this adoption, routine in-scope issue publication, branch push, claim/receipt recording, independent review dispatch, and integration of reviewed local commits into the current integration branch. It does not authorize a GitHub merge, release-to-main merge, formal approval, deployment, protection bypass, spend, or unrelated backlog execution. A `djh00t` workflow-acceptance comment is the controller's engineering child-acceptance receipt from the authenticated account; it is separate from human combined-release and operator acceptance. The profile grants no authority |
| Agent capabilities | Current native tools expose `gpt-6-luna` for bounded mechanical work, `gpt-6.1-sol` for implementation/integration, and `gpt-6-astra` for demanding architecture or adversarial review, with the effort levels in the JSON profile. Recheck availability at dispatch. Ponytail applies; no plugin or dependency installation is part of adoption |
| Knowledge | Helix MCP is optional, project scoped as `djh00t/steve`, with `source_description=codex:steve`; local repository/tracker facts remain authoritative when memory is absent or stale |
| Cleanup and handoff | Stop temporary processes, remove only task-owned rehearsal state, preserve the live journal, report exact revisions and evidence, and end with Went well / Went wrong / Improve next time |

The copied controller supports its documented `existing-ledger-v1` shape. Steve's
485-package inventory is not fed into that adapter because its lifecycle fields
have different semantics. The small external bridge records only active ownership
that predates adoption; it does not duplicate issue lifecycle or release state.
