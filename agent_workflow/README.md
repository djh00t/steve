# Portable dynamic delivery controller

Python 3.10+ and its standard library are sufficient. Helix is optional. The graph
returns eligible work; current chat instructions still govern authority.

## Try the read-only graph

From the repository root:

```bash
python3 -m agent_workflow.demo
python3 -m agent_workflow explain \
  --profile agent_workflow/examples/github_python.json \
  --input agent_workflow/examples/github_python.json
python3 -m agent_workflow explain \
  --profile agent_workflow/examples/gitlab_rust.json \
  --input agent_workflow/examples/gitlab_rust.json
make workflow-check
```

The offline demo returns `ok: true` and named outcomes for routing, claims,
interruption/replay, serial integration and human handoff. It creates and removes
its own temporary journal; it never dispatches an agent.

Both examples return versioned JSON with `status: READY`, explicit prerequisites,
a fingerprint and task-specific `execution`. They are fixtures, not live provider
results. The GitLab example has an explicit cross-project dependency. The strict
check exits nonzero for a failed or empty test suite.

## Live use

Create a repository-specific JSON profile using `CONTRACT.md` and the examples.
Replace all example identities, paths, branches, checks and agent capabilities.
The live GitHub reader uses an already authenticated `gh` CLI and preserves
provider failures as errors/UNKNOWN. Native dependency mode is unsupported;
GitHub tickets use one `workflow-json` block. GitLab execution uses supplied
snapshots only. No credentials are needed for fixture tests.

Each command accepts `--profile PROFILE.json --input REQUEST.json`:

- `explain`: fresh read-only source graph, current claims and receipt-driven route.
- `claim`: refresh sources and transactionally reserve the selected action's resources.
- `record`: idempotently store native dispatch/results or reconciliation evidence.
- `release-check`: validate refreshed admission facts and exact-candidate evidence.

Use `schema_version: 1` and `mode: live` or `fixture` in every request. Live input
cannot inject a clock, snapshot or route progress. SQLite stores attempts, claims
and receipts only; ticket/ledger lifecycle stays in its original source.

The active chat performs native dispatch after a successful **new** claim, then
records the returned native task identity immediately. A repeated/uncertain
attempt never means dispatch again. Inspect the native task or PR and record an
identified reconciliation result first. Heartbeat age cannot release a claim.
See the repository-workflow skill for the handoff sequence.

## Evidence and integration

Admission checks bind ticket, contract, candidate and base revisions, changed
paths, independent review, required check scopes, and actual result artifacts.
Live GitHub admission refreshes PR facts and checks hosted artifact URLs against
current check runs, merged child membership and merge-commit parents; local/native
review artifacts remain producer attestations. R0 supports serial merge commits;
squash/rebase integration histories remain UNKNOWN until an adapter supports them.
No check name or old green result grants permission. Final success means human
handoff, never an agent merge to the default branch.

## Portable adoption

Copy `agent_workflow/`, this repository's portable workflow, ticket template and
repository-workflow skill at one reviewed commit. Record that full commit SHA in
your own profile. Preserve existing agent instructions and dirty work; adapt
profile values rather than copying another project's authority or runtime state.
The controller has no installer, background runner or third-party dependency.

Fixture writes require `--rehearsal-root` under a system temporary directory;
never reuse a live journal for a rehearsal. Retain real journals after interruptions
so the next orchestrator can reconcile them. Remove only your temporary rehearsal
directory when finished. Reverting code does not authorize deleting live claims.

The tested executable kit pin is `91699a6f40810d657e12d01e8a7e9619c6c70606`. Both the GitHub/Python and
GitLab/Rust fixture examples returned READY and passed the offline demo from
separately archived copies of this revision; pre-existing tracked dirty content
and untracked sentinel files were unchanged. These are fixture adoption results,
not a live GitLab integration claim.

Historical protected child PRs cannot be qualified from `merged` alone. The first
live adapter returns UNKNOWN when it cannot establish their protection evidence;
a protected-branch adopter must supply a reviewed adapter for that source before
claiming complete live release admission.
