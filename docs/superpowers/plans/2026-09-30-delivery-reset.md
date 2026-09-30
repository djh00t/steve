# Steve delivery reset

**Base:** `origin/main` at `d6516aa45a6ddad4a66a637fad8f3ccee5761661`

1. Reconcile live GitHub issues, the existing backlog index, and the 485-row FSM export into one repository-owned JSON inventory. Preserve M0-M8 scope and make unknown dispatch metadata explicit.
2. Add a standard-library dispatcher that validates predecessors, accepted input evidence, timeboxes, acceptance commands, ownership, cycles, and conservative path conflicts before deriving the dispatchable frontier.
3. Add focused tests for readiness, stale closed-issue state, ambiguous metadata, graph defects, and path conflicts; refresh the work-package and delivery-plan docs.
4. Run the focused test, dispatcher validation, and repository `make check`; leave the exact diff and evidence for integrator review.
