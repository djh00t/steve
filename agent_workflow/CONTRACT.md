# Agent workflow JSON contract, version 1

This is a data contract for a stdlib Python controller run by the active release
chat. The tracker owns briefs and explicit dependencies; Git/forge owns branches,
revisions and merges; the existing ledger owns lifecycle. The graph is rebuilt
in memory from observations. SQLite under `coordination_root` stores only attempts,
claims and receipts. No snapshot, label, Helix projection or journal row grants
permission or becomes a second lifecycle writer.

All JSON documents have integer `schema_version: 1`. Reject unknown versions,
missing required fields, duplicate keys, invalid types and unknown enum values.
An identity is exactly `{source_type, project, object_type, object_id, revision}`;
all five values are nonempty strings. Revision is the source's opaque exact
revision: ticket semantic hash of body plus native brief relations, Git commit SHA,
or ledger record revision. A pointer uses only the first four fields; a
dependency/contract edge carries its required `accepted_revision` beside the
`target` pointer. IDs are never
inferred from URLs, labels, order, hierarchy, prose or semantic similarity.

## Profile (`--profile`)

`profile_id` identifies a reviewed profile revision. `projects` maps project
keys to `{tracker: {source_type, project, dependency_source}, git:
{source_type, project, default_branch, release_branch}, ledger:
{packages_path, active_path, reader}}`. `dependency_source` is exactly
`workflow-json` or `native` for each adapter; never merge two editable lists.
Each project has `lifecycle_sources`, mapping package-key prefixes to `ledger`
or `tracker-acceptance`. `tracker.acceptance_actors` is a reviewed list of
native account logins. Existing AB records use the ledger. New WF records
use tracker state plus an identified native acceptance comment. Closing an
issue alone cannot mean accepted. The reader fetches the comment, author,
issue link and revision. Its author login must equal the ticket's concrete
`acceptance_owner` login and occur in `acceptance_actors`.
Missing or unverifiable acceptance is UNKNOWN. Unmatched keys are UNKNOWN.
`reader` names the existing authoritative ledger reader; an adapter
must inspect its actual schema before claiming support. `coordination_root` is
the local journal directory. `max_source_age_seconds` is a positive integer.
`checks` maps reviewed check IDs to `{command, cwd, environment}`; commands are
reviewed profile data, never copied or executed from tickets. `capabilities`
lists available `{model, efforts}` pairs for active-chat route selection; runtime
availability is rechecked before dispatch.
`protected_resources` lists canonical shared resource names. A profile is
configuration and routing, never standing spend, live-operation or merge authority.

## Work package (one `workflow-json` fenced block in required ticket body)

Required metadata: `schema_version`, `key`, `outcome` (nonempty observable
result), `kind` (investigation, contract,
implementation, validation), `release`, `parent` (pointer or null), `requires`
(array), `contracts` (array), `owned_paths` (array), `resources` (array),
`check_ids` (array), `criteria` (array of `{id, expected, evidence_scope}`),
`acceptance_owner` (native account login), `risk` (low, medium, high), `uncertainty` (known,
investigation-needed), and `impact` (known paths/check IDs or unknown). Each
`requires` item is
`{target: pointer, accepted_revision, artifact}`; each contract item uses the
same shape. `release` is a release branch name. `owned_paths` are repository
relative paths; resources use canonical profile names. `check_ids` must exist
in the reviewed profile. `evidence_scope` is local, hosted, integrated or
operational. Parent/child and release membership are separate from `REQUIRES`.
For `workflow-json`, the block is the sole dependency source and the reader
recursively fetches explicitly referenced ticket IDs. For `native`, native
tracker dependency fields are the sole source. Native hierarchy is used when
available; otherwise `parent` in the block is used. Ordinary Markdown remains
human-readable data, never commands.

## Snapshot (`--input` for read-only operations)

Required: `profile_id`, `observed_at`, `sources`, `packages`, `git`, `ledger`,
`references`, `claims`, `receipts`, `evidence`, `reviews`. `sources` has one observation for
every needed project/facet: `{project, facet, observed_at, complete, errors}`.
Facets are `tracker`, `git`, `ledger`, `ownership`; errors are structured
`{code, message}` objects. A missing, stale, failed or incomplete required
facet yields UNKNOWN, never an empty graph or READY. Age is measured at query
time against the profile limit. Provider readers preserve partial errors.

Each `packages` entry is `{identity, metadata, lifecycle}`. `metadata` is the
parsed ticket block plus authoritative native relations. `lifecycle` is the
derived `{state, source, acceptance_receipt}`; state is `open`, `active`,
`accepted`, `rejected` or `unknown`. For `tracker-acceptance`, a terminal
state requires a closed issue plus a fetched `acceptance_receipt` containing
`identity` (native comment identity), `parent` (issue pointer), `author_login` and the exact comment
`body`. The body contains exactly one fenced `workflow-acceptance` JSON block:
`{schema_version: 1, target: pointer, accepted_revision, status:
accepted|rejected, artifact, candidate_sha, base_sha}`. The target must name
the parent issue and `accepted_revision` must match its semantic brief revision.
The comment must be from the same provider and project, and its parent must
match the issue. The comment revision is independently observed by the tracker reader. Issue
semantic revision excludes issue state and comments, preventing an acceptance
or close event from changing the brief it accepts. This native comment is
distinct from SQLite operation receipts. Contradictory evidence is UNKNOWN.
The snapshot is a read-only observation. `git` contains
`{project, default_head, release_branch, release_head, release_pr}`. `ledger`
contains observed package records and active pointer; it is not copied to
SQLite. Current nonterminal ledger entries lacking resource fields conservatively
reserve the whole project/shared protected set until ownership is reconciled;
absence of owned resources is never interpreted as free capacity.
For `ledger` lifecycle, match exactly one record by project and package key;
map its known state to the observed lifecycle. Missing, ambiguous or
contradictory records are UNKNOWN. `active-wp.json` is usage attribution, not
an execution claim; a pointer to an accepted record does not reserve work.
`references` contains observed contract artifacts as `{identity}` entries;
every `contracts` edge must resolve to an entry at its accepted revision or
the answer is UNKNOWN. A contract reference is not a `REQUIRES` edge.

`claims` and `receipts` are observations of the local journal, not lifecycle.
An attempt has `attempt_id`, package identity, exact brief revision, starting
base SHA, owner, route (`inline`, `delegated`, `investigation`), current step
(`investigate`, `contract`, `implement`, `verify`, `review`, `integrate`,
`handoff`), step result (`pending`, `passed`, `failed`, `unknown`) and state
(`intent`, `claimed`, `uncertain`, `recorded`).
A claim adds canonical resources and heartbeat; compare and reserve overlapping
resources in one SQLite transaction. A stale heartbeat requires explicit
reconciliation; it does not release resources. A receipt has unique native
`receipt_id`, attempt ID, stage (`dispatch`, `child-pr`, `integration`,
`handoff`), external object pointer, result and time.
Identical replay is idempotent; same ID with different content is a conflict.
If an external dispatch may have happened without a native receipt, mark the
attempt uncertain and reconcile native task/PR before any retry. External state
and SQLite are never atomic.

`evidence` entries bind `{stage, criterion_id, result, artifact, candidate_sha,
base_sha, environment, configuration, observed_at, check_id}`; result is
`passed`, `failed` or `inapplicable` with reason for inapplicability. `reviews`
bind reviewer, independence, verdict, candidate/base SHA and source pointer.
Actual run/artifact IDs are required; a check name or expected outcome alone
is not evidence. Revision changes to ticket, prerequisite, contract, profile,
candidate or base invalidate associated readiness/evidence. A pure refresh of
`observed_at` with identical semantic revisions and facts must leave the graph
fingerprint unchanged. Canonical JSON with sorted keys and stable record order
may be SHA-256 hashed for that fingerprint; include profile revision, semantic
source revisions, base and active claims, but exclude observation timestamps.
Hash the canonical profile content itself; `profile_id` alone is insufficient
when a file is edited without changing its name.

## Operations and fail-closed answers

CLI form: `python -m agent_workflow <explain|claim|record|release-check>
--profile PATH --input PATH`. Input JSON has `mode` (`live` or `fixture`),
`target` (qualified pointer), `snapshot` (fixture mode only), plus operation
fields. `claim` requires `expected_fingerprint`, `attempt_id`, `owner` and
`authority_reference`; `record` requires a receipt/evidence payload;
`release-check` requires an admission manifest with child PR and final release
candidate/base. Live mode reads sources immediately; fixture mode has no
external side effects and supports injected `as_of` for reproducible checks.
Every output, including validation failure, is
versioned JSON. Failure shape: `{schema_version: 1, ok: false, error:
{code, message, details}}`; no traceback or partial success object on stdout.
`explain` returns `READY`, `BLOCKED` or `UNKNOWN`, direct/transitive `REQUIRES`,
cycles, source revisions/currentness, blockers and fingerprint. READY requires
all sources current and complete and every accepted prerequisite at its bound
revision. A current rejected/unfinished prerequisite or overlap is BLOCKED;
missing/stale/failed facts are UNKNOWN; a fully observed cycle is BLOCKED. If
both known blockers and unknown required facts exist, return UNKNOWN and include
both in reasons. Parent
alone never blocks. Cross-project edges require explicit `REQUIRES` and an
observed target with its own source coverage.

The active orchestrator checks current chat authority before `claim` or an
external action. `claim` guards ticket/contract/base/current ownership and
protections and records the caller's current authority reference as audit
context; it cannot verify chat authority or grant permission. It records a
conditional claim or structured
conflict. `record` writes an idempotent native receipt/evidence after verifying
the attempt and exact revisions. `release-check` checks the final release head,
child PR target, child/head/base revisions, changed paths, prerequisite/contract
revisions, independent review, required check results and every criterion's
artifact/environment/configuration. It rejects agent formal approval, deploy,
protection bypass and release-to-default-branch merge. It can produce a human
handoff; only the human merges the release PR to the default branch.

Examples: `examples/github_python.json`, `examples/gitlab_rust.json`, and
`examples/invalid_missing_criteria.json`. Each contains `profile` and `input` so fixture
readers can run without network or credentials. GitHub readers fetch issue bodies,
native explicit dependencies and semantic revisions, Git/base, PR and existing
ledger/ownership observations with pagination and partial errors. GitLab fixture
readers provide the same contract
without requiring GitLab credentials.

## CLI execution and evidence scope

A reviewed check may add `evidence_scope` (`local`, `hosted`, `integrated`, or
`operational`); absent means `local`. Admission requires results from that exact
scope. A locally asserted result cannot satisfy a hosted check.

`explain` composes the ready package's execution route and returns it as
`execution`, including reviewed check IDs, transitions, capability and next-step
class. Live progress comes from stored native receipts, never injected request
observations. Fixture input may supply observations to rehearse route changes.

Fixture mutation commands require `--rehearsal-root PATH`, a separate directory
under the system temporary directory. The CLI marks it `.agent-workflow-fixture`,
rejects existing unmarked nonempty directories, and never uses the profile's live
coordination root for fixture writes. Use the same flag for explain/claim/record
so their profile fingerprint is stable. Live mode refuses marked rehearsal roots.
The CLI never invokes native agents or executes ticket/profile commands.

Admission manifests, reviews and evidence carry `profile_revision`, the SHA-256
of the entire canonical profile (sorted JSON keys, compact separators, UTF-8).
Use `agent_workflow.admission.profile_revision(profile)` to produce it at evidence
capture. A changed profile invalidates old admission evidence even with unchanged
check IDs. Required checks come from the current route, including impact widening.

A final manifest includes `release` with actual PR identity/target/head/base and
a complete source-attested `expected_children` set. This release membership is
separate from `REQUIRES`. Ordered `children` contain validated package observations,
child admission manifests, and sourced integration receipts. Their serial chain
runs from `integration_base_sha` to the exact final candidate; the release PR base
can independently advance on main. The live GitHub reader verifies actual merge
parents and membership, and requires the candidate to include current main.
Only merge-commit integration is supported by this first live adapter.
