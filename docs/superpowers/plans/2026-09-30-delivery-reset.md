# Steve delivery reset

**Base:** `origin/main` at `d6516aa45a6ddad4a66a637fad8f3ccee5761661`

1. Reconcile live GitHub issues, the existing backlog index, and the 485-row FSM export into one repository-owned JSON inventory. Preserve M0-M8 scope and make unknown dispatch metadata explicit.
2. Add a standard-library dispatcher that validates schema version, predecessors, accepted input evidence, timeboxes, acceptance commands, ownership, cycles, and conservative path conflicts; active known paths reserve ownership and unknown active scope blocks dispatch.
3. Add focused tests for readiness, stale closed-issue state, ambiguous metadata, graph defects, active ownership, schema rejection, and path conflicts; refresh the work-package and backlog docs.
4. Run `make workflow-check` for the portable demo and nonempty inventory validation, then run repository `make check`; leave the exact diff and evidence for integrator review.
