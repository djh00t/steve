# Work-package issue body template

Reusable body format for the repository's existing issue tracker. Copy the
sections into a ticket and replace authoring instructions with concrete values.
Use native parent/dependency/release fields where supported; otherwise use one
documented field in the existing ticket format. This is not an installed issue
form or executable validator; see [gates](PORTABLE_WORKFLOW.md#gates-and-dispatch).
Read `REPO_WORKFLOW_PROFILE.md` for local commands, ownership and authority.

## Identity and outcome

Use title `[<project package key>] <observable outcome>`. State package key, kind
(investigation/contract/implementation/validation), beneficiary and success
condition. Link the originating request and relevant decisions. Put the parent
epic, release milestone and blockers in the tracker's designated fields; do not maintain a
second editable dependency list here.

## Scope and ownership

List files/modules the worker may change, new artifacts it produces, shared
resources it may use and exclusions. Explain any interface affected outside the
owned files. Identify the release orchestrator as acceptance owner; the specific
worker/attempt identity is assigned when claimed, not fabricated in a draft.

## Prerequisites and contracts

For each native dependency, identify its required artifact and accepted revision.
Link canonical input/output, error, identity/idempotency and persistence contracts.
State compatibility or migration effects. Missing decisions block readiness or
become a separate bounded investigation. Dependency-free work says why it is
independent. Do not infer independence from different filenames alone.

## Acceptance criteria and BDD

Number observable criteria and map each to Given/When/Then scenarios. Cover the
intended behaviour and material failure/boundary cases. State whether evidence
must be local, hosted, integrated, scheduled or operational. Name the acceptance
decision required; passing tests alone do not grant acceptance.

## Verification and mutation

Provide actual command, working directory, environment/prerequisites, expected
result and linked criterion for every required check. Name any producer that
introduces a new check. Include the smallest meaningful end-to-end route and a
fault/mutation the check must detect. Document justified inapplicability for
non-code work. Do not invent existing commands or require a full suite for a tiny
fix unless impact or repository gates justify it. Commands must be reviewed
before execution, not blindly trusted because they appear in a ticket.

## Execution route and boundaries

State inline/delegated/investigation route, risk, active-time estimate (target
5-10 minutes), external waits and proposed model/reasoning for delegation. List
spend, credential, live-operation and publication permissions that apply, with
source and limits. Use explicit none/not-needed where accurate; missing authority
is not implied approval. Record the exact release branch/PR/MR and starting base at
claim time. Split work whose outcome cannot fit the target safely.

## Delivery, demonstration and recovery

Name documentation updates and the child PR/MR's parent release. Explain how the
reviewer can see/test the behaviour, expected output and cleanup. State rollback
or recovery, including data/compatibility constraints. Child acceptance/merge is
owned by the orchestrator; the human owns release-to-default-branch merge.

## Evidence and completion

At delivery, append child/head and tested base SHAs; commands and actual results;
run/artifact links with environment and timestamps; spec/quality review verdicts;
integration result; limitations and exact remaining blockers. Required but missing
evidence stays missing, not passed. Identify active processes/resources and their
cleanup owner. Preserve links to failed attempts and fixes where relevant.

## Session self-assessment

- Went well: supported outcome or effective approach.
- Went wrong: observed failure, waste or limitation; none observed is valid.
- Improve next time: scoped adjustment and how its benefit could be tested.

Reuse the agent report as the assessment source rather than copying complete
transcripts. A report is evidence for candidate learning, not automatic policy
promotion or independent proof of success.
