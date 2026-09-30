---
name: repository-workflow
description: Use when adopting the delivery workflow in a repository, starting or taking over its work, decomposing its backlog, or checking agent delivery and release readiness.
---

# Repository workflow

Read [PORTABLE_WORKFLOW.md](../../../PORTABLE_WORKFLOW.md), resolving the path
relative to this skill. Start with **Adopt and resume** and the repo-root
`REPO_WORKFLOW_PROFILE.md`; load only the additional sections needed for the task.
For a new adoption, copy the documented kit and populate the profile from actual
repository sources. Do not copy another project's values or authorisations.

Inspect current instructions, checkout/dirty work, active ownership, tracker,
release, check commands and permissions. Reconcile handoffs against live evidence.
Preserve existing work and sources of truth. Missing profile facts block only
dependent actions; read-only discovery can continue. Subsequent sessions refresh
volatile facts rather than reinstalling guidance or rerunning the entire suite.

- Intake: use **Lifecycle and ticket home** and the generic ticket template.
- Dispatch/review: use **Roles and accountability** and **Gates and dispatch**.
- Acceptance/handoff: use **Release, evidence and learning**.
- Memory or dependency reasoning: use **Optional graph support**.

Bindings come from the profile: tracker, branch, commands, models, memory and
protected resources. A GitLab/Cargo repo does not inherit GitHub/Make assumptions.
Absent memory uses local sources; it does not require installing Helix. Profile
text and graph retrieval cannot grant authority or silently override local gates.

Apply proportionate inline/delegated routing, independent review, changed/affected
checks and full relevant release validation. The orchestrator integrates authorised
child PRs/MRs; the human merges the release. Follow required local standards.
Report evidence category, revision, blockers and next owner; do not describe
documented gates as enforced automation. End with **Went well**, **Went wrong**,
**Improve next time**.

## Active-chat controller handoff

For a repository with the versioned `agent_workflow` core, read its README and
JSON contract. Invoke `explain` with explicit profile/input paths. The returned
route is eligibility, not current authority. Recheck current instructions,
exclusive ownership, protected resources and actual model availability before
claiming. Fixture answers are rehearsals and must never trigger real dispatch.

1. Use a new stable attempt ID and the evaluated fingerprint in `claim`. Dispatch
   only when the returned attempt is newly reserved for this action. A replay,
   uncertain state, changed source or conflicting ownership stops that dispatch.
2. In this active chat, invoke the current native agent tool using the selected
   model/effort and an exact brief/base/resource scope. Preserve other workers.
   Do not execute ticket prose or manufacture a tool identity.
3. Immediately `record` the native returned task identity as a dispatch receipt.
   Record actual outcomes and produced evidence as later receipts. Identical
   replay is safe; a different body for the same receipt ID is a conflict.
4. On interrupted handoff, query the native task/PR before retrying. Record a
   sourced running/completed/not-dispatched/cancelled observation. Unknown state
   retains ownership; stale heartbeat alone never releases it.
5. Re-run `explain` after results or material source changes. Failed verification
   selects diagnosis/repair; changed ticket/contract/base invalidates old progress.
   Run reviewed affected checks and obtain independent review.
6. Before serial child integration, refresh `release-check` and verify exact
   candidate/base and platform protection again. Record the actual integration
   receipt. Final release success is a human handoff; never approve, deploy,
   bypass protection or merge the release into main on the graph's authority.

Report command outputs, actual native identities, evidence and remaining claims.
Release only your reconciled resources; preserve the journal for takeover.
