# Portable agent delivery workflow

Version 1, 2026-09-28. Reusable operating contract for starting or taking over a
repository. Adoption changes guidance, not running services or CI.
This is a documented process and skill; automated enforcement remains separate work.

## Adopt and resume

Copy this file, [WORK_PACKAGE_TEMPLATE.md](WORK_PACKAGE_TEMPLATE.md), and
`.agents/skills/repository-workflow/SKILL.md` preserving their relative layout.
Create a repo-root `REPO_WORKFLOW_PROFILE.md` using the fields below. Add a link
to the skill/profile in the repository's existing agent entrypoint; merge the
entry rather than replacing its instructions. Other agent runtimes can read the
same Markdown even if their skill discovery differs. Do not copy another repo's
profile values, permissions, backlog, identities or runtime state.

Record the adopted version and source revision in the profile. While the source
is uncommitted, record that explicitly and pin the committed source before claiming
a reproducible distribution. Review version updates as ordinary changes; no
silent global upgrades, installer, new tracker or graph runtime is required.

When the user requests delivery, that request authorizes routine in-scope commits,
pushes, ticket publication, and PR/MR creation or updates needed to complete it.
Do not ask again for these steps. An explicitly planning-only request remains
planning-only. The human retains release-to-default-branch merge; formal approval,
deployment and protection bypass require separate explicit authority.

At every start or takeover:

1. Verify repository identity, root, worktree, branch, HEAD and working changes.
   Read applicable instructions and the profile. Verify remotes/default branch;
   never assume the branch is named `main` or the host is GitHub.
2. Read the existing backlog/tracker, current release PR or merge request, claims,
   relevant contracts, checks and recent handoff. Inspect affected active resources.
   Do not reset dirty work, duplicate a release or adopt another agent's claim.
3. Reconcile the handoff with current issue revisions, code, CI and permissions.
   A prior green run applies only to its candidate/base/inputs/environment. Preserve
   ongoing ownership; a stale heartbeat alone does not authorise stealing work.
4. For first adoption, fill the profile from inspected sources and list gaps and
   conflicts. Keep read-only discovery moving; block only work depending on a
   missing contract, authority or safe test route. Do not invent successful checks.
5. Select the smallest ready outcome and inline/delegated route. Start execution
   only within current task authority. Subsequent sessions refresh changed or
   volatile facts rather than reinstalling the process or rerunning all tests.

Takeover output: current state and evidence, unresolved conflicts, existing owners,
next ready work, permitted actions, and cleanup responsibilities. This can be a
short handoff in the existing tracker; it does not require another report file.

## Repository profile contract

Use a short Markdown table. Each value names its inspected source and date when
volatile. Mark unknown/unavailable/not applicable with a reason. Unknown required
facts block dependent execution, not all planning. Never put credentials here.

| Field | What the repository supplies |
| --- | --- |
| Adoption | Core version, source revision/location, local owner and adaptations |
| Identity and ingress | Stable repository identity, tracker URL, primary input chat/channel and owner |
| Delivery sources | Backlog, architecture/contracts, ticket-ID scheme, authoritative execution ledger and claim writer |
| Release | Verified default branch, release/child naming, active release reference discovered at claim, integration and human merge owners |
| Checks | Exact commands, working directories, prerequisites and scope selection; separate child, integrated-release and post-merge routes |
| Evidence | CI provider/triggers, required checks/reviews, artifact locations and acceptance categories |
| Runtime boundaries | Protected files/services, active ownership source, environment/credential setup references and cleanup rules |
| Authority | Current authorisation sources and limits for publication, child merges, release merges, deployment and spend |
| Agent capabilities | Available runtimes/models/efforts and role mapping; required skills or documented unavailable-capability gaps |
| Knowledge | Optional memory service, project scope/provenance, offline source and pending-write handling |

Current authoritative instructions govern. The profile binds this process to local
capabilities; it cannot grant permissions or silently weaken required controls.
Surface genuine conflicts with existing policy and resolve only affected actions.
No fixed filesystem root, test runner, model vendor, tracker or memory backend is
part of the reusable contract. Discover the actual tool interface before use.

## Roles and accountability

Use one ingress role per project, one orchestrator per active release, temporary
implementers and an independent reviewer/QA as needed. The orchestrator can do
small fixes inline; someone else reviews its changes. Architecture and improvement
analysis are assignments, not necessarily permanent agents. Start small; concurrency
follows ready independent work, review capacity and CPU/memory/disk headroom.

R = responsible, A = accountable, C = consulted, I = informed. One A per activity.

| Activity | Human | Ingress | Orchestrator | Implementer | Reviewer/QA |
| --- | --- | --- | --- | --- | --- |
| Outcomes, priority and authority | A | R | C | I | C |
| Intake and epic decomposition | C | A/R | C | C | C |
| Contracts and work-package readiness | C | R | A | R | C |
| Implementation and focused checks | I | I | A | R | C |
| Independent review | I | I | A | C | R |
| Child acceptance and integration | I | I | A/R | C | C |
| Release validation and handoff | C | I | A/R | C | R |
| Final release PR/MR merge | A/R | I | C | I | C |
| Authorised rollout and operational acceptance | A | I | R | R | R |
| Policy changes beyond approved adaptive bounds | A | R | R | C | R |

## Lifecycle and ticket home

Idea -> outcome/triage -> affected architecture and contracts -> dependency-ordered
work packages -> child implementation/review -> release integration/QA -> human
release merge -> authorised rollout/operational acceptance -> learning.

Use the repo's existing issue tracker as the canonical ticket home. Organise
programme/epic/package and release membership with its native features. Explicit
dependencies are different from hierarchy; similarity is not a dependency.
If a required native field is absent, use one documented field in the existing
ticket format. Do not add another database. Before publication, keep drafts in
the existing backlog artifact. Preserve established ticket IDs and references.

Keep one authoritative writer/source per fact: briefs and relationships in the
tracker; execution claims in the existing agreed ledger; revisions/merges in Git
and the forge; results in run artifacts; permissions in current user instructions.
If the tracker already owns claims, use it. If no claim store exists, a single
orchestrator-maintained assignment field in the tracker is sufficient initially.
Migrate ownership explicitly; labels, memory and dashboards are projections.

Refine only the next useful wave. Target 5-10 active minutes per complete slice;
record CI/operator waits separately. Split by observable behaviour, not arbitrary
file counts. A regression test and fix usually form one passing child change.
Investigate material unknowns before promising executable packages.

Use [WORK_PACKAGE_TEMPLATE.md](WORK_PACKAGE_TEMPLATE.md). Briefs must resolve
identity/outcome, scope, dependencies/contracts, acceptance/BDD, checks/mutations,
execution/authority, release/demo/recovery, evidence and self-assessment. Document
justified inapplicability; a tiny fix need not create ten separate documents.

## Gates and dispatch

| Gate | Required checks |
| --- | --- |
| Draft | Identifiable outcome, source, scope and explicit unknowns |
| Ready | Bounded scope; prerequisites accepted and available on starting base; contract references; observable criteria; real test commands or named producer; ownership and authority known |
| Claim | Fresh brief revision/hash, base and dependencies; no conflicting resource claims; named attempt and route/model/effort |
| Child admission | Correct release target, reviewed scope/diff, exact candidate/base evidence, required checks/reviews, orchestrator acceptance |
| Release handoff | Final integrated candidate, complete relevant release validation, child accounting, runnable try/test instructions, evidence and human merge request |

Start with direct review against these gates. When adding enforcement, use one
small shared contract/validator at readiness and admission with valid/invalid
fixtures. Required issue-form fields alone are insufficient. Reject missing
criteria, cycles, stale revisions, conflicting claims, wrong targets and missing
evidence; accept a justified small inline fix. Revalidate material edits. Never
blindly execute ticket commands in privileged CI. Automated validation checks
structure/evidence bindings; it does not replace acceptance judgment.

Every agent brief includes ticket/revision, release/base, contracts, owned
files/resources, acceptance/checks, current permissions, model/effort and reporting
destination. Workers are not alone and must preserve others' changes. Dispatch
independent work only; serialize shared contracts and release merges. Reuse workers
where context remains suitable, and park idle roles rather than polling.

Choose the least costly available model/effort demonstrated adequate for the task:
small for bounded mechanical work; general-purpose for implementation; stronger
reasoning for uncertain architecture, integration or adversarial review. Escalate
on evidence of uncertainty/failure. Record actual routing; unavailable telemetry
is unknown. Model names belong in the profile, not in this portable policy.

Ponytail, parallel dispatch and subagent-driven development are the preferred
skills when available. Apply them with these user-directed adaptations: inline
low-risk fixes, independent parallel workers, proportionate review and affected
checks. Missing plugins do not justify installation without authority or fabricated
skill use; preserve the process locally and report the capability gap.

Role brief additions:

- **Ingress:** own intake, deduplicate, preserve intent, refine upcoming work and
  route authorised delivery. Return priority, ready candidates and open decisions.
- **Orchestrator:** own claims, route tasks, check real output, accept and serially
  integrate children, produce release evidence and notify the human for final merge.
- **Implementer:** deliver only assigned surfaces against the brief/contracts,
  trace affected callers, test and document; return revisions, results and blockers.
- **Reviewer/QA:** independently assess spec and quality on the actual diff and
  integrated behaviour; return evidence-backed findings, without self-acceptance.
- **Improvement analyst:** compare outcomes/friction, propose bounded changes with
  counter-evidence and evaluation; do not promote policy on your own authority.

## Release, evidence and learning

Every agent PR/MR, including inline fixes, targets the designated release branch
and meets normal repository standards. The orchestrator owns child acceptance and
merges within authorised scope; the human checks and merges the release PR/MR into
the default branch. Child-merge authority grants neither formal approval authority,
protection bypass nor deployment permission. Reuse an existing child where suitable.

Run changed and affected checks for children using the profile's actual commands.
Run complete relevant validation at the release gate. Keep post-merge checks in
their designated environment; report any pre-merge evidence gap honestly. Evidence
reuse requires matching code/base/inputs/configuration/environment. No CI means no
hosted evidence, not green. A child integrated into a release is not yet released.

Handoff includes release link, final revision, child links, actual test/integration
results, reproducible setup/try/test steps and expected output, risks/rollback and
remaining operational steps. Use the existing PR standard/template. Stop temporary
processes and retire unused worktrees safely after preserving work and ownership.

End every agent session with **Went well / Went wrong / Improve next time**, brief
and evidence-grounded; none observed is valid. Capture unnecessary waits, duplicate
checks, model/dispatch overhead and rework. Evaluate improvements on comparable and
held-out tasks before promotion. Adapt routing, batching and check selection within
approved bounds; never learn away authority, required evidence or human release merge.

## Optional graph support

Skills supply the procedure; the repo profile binds it to tools; the tracker/ledger
supplies state. A memory graph supplies sourced relationships and lessons. The
workflow must remain usable without a graph connection.

Project-scoped records can describe work items, releases, contracts, PR revisions,
test runs, attempts and workflow revisions. Relationships need explicit source IDs,
revisions, observed time and validity. Namespace identities by repository/provider
to prevent ticket-number collisions. Retrieve cross-project prerequisites only
when explicitly linked; general lessons do not transfer project permissions.

Start with a read-only **explain readiness** query over authoritative records:
what can run, what blocks it, which evidence is missing/stale, and why. Add graph
traversal when multi-hop dependencies justify it. Reconcile idempotently, expose
staleness and refresh live authority/claims before acting. Memory cannot grant a
merge, claim or promotion. No new workflow engine is required for initial adoption.
