# Work-package readiness and parallel delivery

GitHub issues are the source of detail. The [backlog index](backlog-index.md) links reviewed packages; see the [delivery plan](delivery-plan.md) for phases and combined-head exits and the [contract registry](contracts/README.md) for planned contract artifacts. Workbench is coordination-only; its cards contain short status and issue links.

## Canonical dispatch inventory

[`work-packages.json`](work-packages.json) is the repository-owned reviewed projection. The portable repository workflow tracker is the single mutable authority for claims, transitions and dependency changes; regenerate this projection after those facts change rather than editing it to claim work. It records live issue readiness, reconciled readiness and imported delivery state separately, so a stale issue label is visible without overriding verified dependency evidence. Every package has a stable ID, kind (`leaf` or `coordinator`), predecessor IDs, required accepted input revision, evidence and source policy, owned paths, one-writer assertion, active-minute estimate and exact acceptance command. `null` means the field has not been reconciled and makes the package ineligible; it is never an implied empty list, zero-minute estimate or accepted input.

Run `python3 scripts/dispatch_work_packages.py` for the count and frontier, or add `--json` for the derived FSM and diagnostics. The derivation requires schema version 1, rejects missing predecessors and cycles, requires every predecessor input to have an exact 40-character accepted revision with evidence on the source base, and conservatively treats exact, path-prefix and glob ownership overlap as a conflict. `IN_PROGRESS` and `CLAIMED` packages reserve known paths even when they are not candidates; an active package with unknown paths blocks the full frontier until ownership is reconciled. A closed issue is not product acceptance and cannot remain READY.

Run `make workflow-check` for the portable controller demo and inventory verification. The aggregate fails on command errors, empty output, a failed demo result, or an empty inventory; a zero dispatch frontier is valid when active ownership is unknown.

`acceptance_command_kind` distinguishes a test the package must introduce from a dependency command it consumes. An introduced selector is a delivery obligation, not passing evidence. A consumed selector must already be verified on the required source before dispatch.

## A package is one small, complete outcome

Target 5–10 minutes of active development and review by a junior familiar with Rust and the repository, after prerequisites are available. Estimate active work separately from dependency installation, first compilation, CI queues and review waits. This is a sizing target, not a guarantee. If probing or implementation shows more than ten active minutes, split at the next independently testable behaviour before dispatch/continuation; do not disguise a larger package with a ten-minute label.

A schema, endpoint, UI screen or new adapter is not automatically a ten-minute package. Split by an observable behaviour or a concrete reviewed contract. Include the necessary test and documentation in that slice. Do not manufacture tiny setup-only tasks that leave unused scaffolding. Discovery packages produce a specific decision, fixture, compatibility result or follow-on decomposition; they do not pretend the undiscovered implementation is ready.

## Required issue contents

- Kind: **decision**, **implementation**, or **integration gate**.
- Outcome, non-goals and estimated active minutes.
- Exact prerequisites, including the parent contract revision and dependencies that must be merged.
- Starting references: source files/symbols, specification section and fixture.
- Exact owned files/symbols, one writer, and any shared-file integrator/serialization rule.
- Exact input/output contract, or the decisions and evidence a contract package must produce.
- A short implementation sequence with no unexplained design choice left to the junior.
- Given/When/Then acceptance examples tied to one shared integration scenario.
- Test command, expected result, and a specific fault/mutation that would expose a weak test.
- Documentation changes, validation evidence, and ready-for-review PR delivery criteria.

Use [the testing policy](testing.md). Documentation/decision packages validate their evidence and downstream contracts through review; they do not need invented runtime tests.

## Readiness states

- **READY:** the package can start from its candidate base with its scope, ownership, inputs and validation path understood. An **implementation** leaf must have its required decisions accepted and predecessor artifacts merged. A **decision** leaf marked READY may prepare a proposal and evidence only; READY does not mean its choice is accepted or authorize downstream implementation. The accepting authority must be identified in the issue or by the user; do not infer or invent an approver. A producer may add its own test/command and prove it on its candidate branch; consumers require that producer merged and the command runnable on their starting base.
- An **integration gate** is READY only after its implementation predecessors are merged and its command runs on the candidate base. It executes the composed scenario and reports the exact tested combined SHA; it records evidence for that scenario and does not guarantee behavior outside its assertions.
- **BLOCKED:** the issue is fully described, but a named predecessor or contract has not landed. It is not dispatchable yet.
- **DEFERRED:** deliberately post-MVP or experimental. Complete the bounded discovery/contract package only when that scope is activated; further implementation leaves are sized from the resulting evidence.
- **IN_PROGRESS / DELIVERED / ACCEPTED:** owned, reviewable PR, and merged with required evidence respectively. A closed historical issue is not proof of a new integration guarantee.

The review includes planned downstream leaves where contracts are not yet frozen. Those implementation leaves remain BLOCKED until the exact accepted contract, test and command are available. A decision package freezes its question, evidence, output and affected consumers before dispatch, then publishes the proposed contract for explicit acceptance. No junior should invent missing security, money, API or UI decisions.

Before dispatch, probe assumptions about existing daemon behavior that the package depends on. A missing new test/command can be created within its producer package. If the probe exposes an in-scope defect, include the smallest fix in that same useful outcome and update owned paths; do not create an untested prerequisite just to make the test package look ready. For example, STV-TST-01 (#72) owns the smoke outcome and its `127.0.0.1:0` listener-guard correction. A defect outside the package boundary remains an explicit predecessor.

## Parallelism

Parallel means dependency-independent and write-independent. Packages in the same milestone are not necessarily parallel-safe.

1. Start only from a fresh base containing every predecessor, or an explicitly coordinated PR stack.
2. Use one isolated branch/worktree per package and one writer for each owned file. Different symbols in a shared Rust module still need serialization unless an integrator explicitly owns the combined change.
3. Shared contract and schema decisions land before consumers. For a shared registration file, one integrator owns serialization, but each useful endpoint slice should implement, test and register its route in the same change; do not defer all route registration into a later registration-only wave.
4. Compute the next wave from merged dependencies and non-overlapping ownership. Test execution alone may run concurrently against isolated processes/data.
5. The integration owner reruns the composed scenario on the actual combined head. Green isolated PRs do not prove that their merge compiles or behaves correctly.

One named integrator owns each shared path. Each leaf gets its focused acceptance command and review. After the leaves are combined, freeze the candidate SHA and run one broad review and release gate; fixes receive a scoped rereview before the candidate is frozen again. Engineering fixtures and working contract baselines land before runtime consumers. The user receives one combined release demonstration, and the FSM is regenerated after every verified milestone.

Do not start all READY issues at once: select a disjoint set of ownership paths. Use explicit dependencies for ordering, not assumptions in a title. Parent epics remain open until their children and the composed acceptance scenario are accepted.

## Definition of done

The stated behaviour works; its acceptance scenario fails for the named fault and passes for the implementation; the affected documentation agrees; relevant checks pass on the PR head; no useful acceptance requirement is replaced by a mock-only or log-only assertion. Publish a ready-for-review Conventional Commit PR with `Fixes #<issue>`, head SHA and CI evidence. Do not merge or approve on behalf of the user.

No placeholders or knowingly incomplete behaviour count as delivered. A decision package is complete only when its evidence and proposed artifact are reviewed and explicitly accepted by the identified authority; without acceptance, downstream implementation remains BLOCKED.
