# STV-M6-02 (#187): Logical selection precedence

**Status:** Proposal only; pending David/Cos acceptance. **Evidence base:** `4472ca59896465fcf27b0d1df1d5218552d80efd`. This base has no `RoutingEngine`; this is policy, not runtime behavior.

Source: `docs/specs/2026-09-26-steve-gateway.md` §8 separates logical model/task choice from provider/account choice; `docs/plans/2026-09-26-steve-mvp.md` §M6 includes the routing engine; live #187 brief supplies the acceptance cases.

## Proposed pipeline

First normalize supplied selectors using request intent, resolved deployment/user/client context, and Steve configuration; these are conceptual sources, not proposed public fields. Missing selectors are absent. A supplied explicit model and alias are both resolved before comparison. Malformed/unknown supplied selectors, or missing, invalid, disabled, or unauthorized fixed-target references when used, are errors, never treated as absent. Alias resolution is one direct lookup; no alias chaining or recursive profile inheritance.

| Order | Selection source | Proposed rule and outcome |
|---|---|---|
| 1 | Explicit logical model | A supplied model other than the exact, case-sensitive literal `auto` is a fixed target. Validate during normalization, then select it. The literal `auto` means no fixed model; proceed through profile rules, requirements, then the RoutingEngine. It is not a catalogue identifier. |
| 2 | Alias/profile context | An alias resolves to a fixed logical target. A profile supplies rule order and an optional default target, used only after no rule matches. If explicit fixed model and alias are both supplied, they must resolve to the same target or return `conflicting_model_selection`. A fixed model or alias skips rules and profile default; profile context does not override either. |
| 3 | Profile rules, then profile default | If no explicit model/alias fixed a target, evaluate rules in configured sequence; the first matching rule wins. Sequence position is the only precedence/tie-break rule. A matching rule with missing, invalid, disabled, or unselectable target errors; do not try later rules or the profile default. If no rule matches, select the profile's default if present; otherwise continue. |
| 4 | Capability/context requirements | Validate any fixed target against all mandatory requirements. If required metadata is unknown, return `selected_target_unverifiable`; if known and incompatible, return `selected_target_incompatible`. With no fixed target, first construct the candidate set from catalogue targets known to be enabled, logically available, and selectable by the caller in the same resolved context. Unknown logical availability, selectability, or authorization is not presumed eligible and excludes a candidate. Then exclude candidates incompatible with requirements or whose mandatory metadata is unknown. An empty candidate set returns `no_logical_target_matches`. |
| 5 | Auto through Steve `RoutingEngine` (#188/#272) | Pass the complete candidate set remaining after stage 4 and the normalized selection context/requirements to `RoutingEngine`. It must return one member of that set; empty, unknown, or out-of-set results are errors, not a fallback. The accepted engine policy must be deterministic for the same normalized input and catalogue/policy snapshot. An adapter must honor that contract; this proposal selects no vendor SDK or ranking algorithm. |

Advancing to a later stage is explicit fallback only when a selector is absent, a rule set has no match, or no profile default exists. Invalid selectors, conflicting fixed targets, and invalid/unavailable/unauthorized fixed targets fail closed. “Unavailable/unauthorized” here means only logical-target status (such as disabled catalogue entry or permission to select it). Provider/account health, ownership, pool eligibility, and access rejection remain downstream and must not restart logical selection.

## Boundary examples

- Exact, case-sensitive explicit model value `auto` → no fixed model; evaluate profile rules and requirements, then call `RoutingEngine`. If an alias is also supplied, its fixed-target rules still apply.
- Explicit `model-a` plus alias→`model-b` → `conflicting_model_selection`; alias→`model-a` → select `model-a` and validate its requirements.
- First rule matches disabled `model-a` → fixed-target error; do not try rule two, profile default, or auto.
- Required context metadata unknown for a fixed target → `selected_target_unverifiable`; unknown metadata or unknown caller selectability excludes an auto candidate. If that leaves no candidates, return `no_logical_target_matches`.
- No rule/default and no candidate passes mandatory requirements → `no_logical_target_matches`.
- A selectable model later has no eligible provider accounts → downstream outcome; no logical reselection here.

**Evidence:** Runtime and mutation evidence are N/A for this proposal-only decision.

**Acceptance boundary:** No provider/account scoring, eligibility, dispatch, persisted alias/profile schema or storage (#558 owns the pure domain shape; persistence belongs to downstream consumers), or SDK selection. Consumers remain blocked until acceptance and qualification of exact fields, fixtures, and a runnable command. After acceptance, record the accepted revision here and update consumer briefs.
