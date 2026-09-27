# Work-package readiness and parallel delivery

GitHub issues are the source of detail. The [backlog index](backlog-index.md) links the reviewed packages and parent outcomes. Workbench is coordination-only; its cards contain short status and issue links.

## A package is one small, complete outcome

Target 5–10 minutes of focused development and review by a junior familiar with Rust and the repository, once the listed prerequisites are available. Dependency installation, initial compilation, CI queues and review waiting time are recorded separately. This is a sizing target, not a guarantee. If the first failing test or implementation reveals more than ten minutes of active work, split the remaining behaviour at an independently testable boundary before continuing.

A schema, endpoint, UI screen or new adapter is not automatically a ten-minute package. Split by an observable behaviour or a concrete reviewed contract. Include the necessary test and documentation in that slice. Do not manufacture tiny setup-only tasks that leave unused scaffolding. Discovery packages produce a specific decision, fixture, compatibility result or follow-on decomposition; they do not pretend the undiscovered implementation is ready.

## Required issue contents

- Outcome, non-goals and estimated active minutes.
- Exact prerequisites, including the parent contract revision and dependencies that must be merged.
- Starting references: source files/symbols, specification section and fixture.
- Owned files/symbols and shared integration points.
- Exact input/output contract, or the decisions and evidence a contract package must produce.
- A short implementation sequence with no unexplained design choice left to the junior.
- Given/When/Then acceptance examples tied to one shared integration scenario.
- Test command, expected result, and a specific fault/mutation that would expose a weak test.
- Documentation changes, validation evidence, and ready-for-review PR delivery criteria.

Use [the testing policy](testing.md). Documentation/decision packages validate their evidence and downstream contracts through review; they do not need invented runtime tests.

## Readiness states

- **READY:** all inputs exist on the candidate base; no unresolved contract, tool, credential or product choice; the existing prerequisite commands work; ownership is available. A producer may add its own new command/test on its candidate branch and must prove it there; a downstream consumer requires that producer merged and its command runnable on the starting base.
- **BLOCKED:** the issue is fully described, but a named predecessor or contract has not landed. It is not dispatchable yet.
- **DEFERRED:** deliberately post-MVP or experimental. Complete the bounded discovery/contract package only when that scope is activated; further implementation leaves are sized from the resulting evidence.
- **IN_PROGRESS / DELIVERED / ACCEPTED:** owned, reviewable PR, and merged with required evidence respectively. A closed historical issue is not proof of a new integration guarantee.

The review includes planned downstream leaves where contracts are not yet frozen. Those are estimates and remain BLOCKED; the prerequisite owner must replace decision-dependent details with the exact approved contract, test and command before marking them READY. No junior should have to invent missing security, money, API or UI decisions to finish a leaf.

## Parallelism

Parallel means dependency-independent and write-independent. Packages in the same milestone are not necessarily parallel-safe.

1. Start only from a fresh base containing every predecessor, or an explicitly coordinated PR stack.
2. Use one isolated branch/worktree per package and one writer for each owned file. Different symbols in a shared Rust module still need serialization unless an integrator explicitly owns the combined change.
3. Shared contract, migration, fixture, module-registration and management-router edits land first. Feature workers consume the merged contract and own separate feature files. They must not independently edit the same registration table.
4. Compute the next wave from merged dependencies and non-overlapping ownership. Test execution alone may run concurrently against isolated processes/data.
5. The integration owner reruns the composed scenario on the actual combined head. Green isolated PRs do not prove that their merge compiles or behaves correctly.

Do not start all READY issues at once: select a disjoint set of ownership paths. Use explicit dependencies for ordering, not assumptions in a title. Parent epics remain open until their children and the composed acceptance scenario are accepted.

## Definition of done

The stated behaviour works; its acceptance scenario fails for the named fault and passes for the implementation; the affected documentation agrees; relevant checks pass on the PR head; no useful acceptance requirement is replaced by a mock-only or log-only assertion. Publish a ready-for-review Conventional Commit PR with `Fixes #<issue>`, head SHA and CI evidence. Do not merge or approve on behalf of the user.

No placeholders or knowingly incomplete behaviour count as a delivered implementation. A decision package is complete only when its concrete output is reviewed and every dependent issue has the required contract or remains explicitly blocked.
