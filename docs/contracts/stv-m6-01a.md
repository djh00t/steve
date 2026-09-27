# STV-M6-01A (#549): ScoreComponent result envelope

**Status:** Proposal for David/Cos review; not accepted policy.
**Scope:** Result representation only. No component formula, weight, candidate field, or routing policy is decided here.

## Proposed contract

| Decision | Proposal |
|---|---|
| Envelope | `ScoreComponentResultV1 { version: 1, status: "known" | "unknown", value_micros?: i64 }`; wire JSON encodes `value_micros` as a canonical base-10 integer string. |
| Version | Required integer `version`, exactly `1` for this closed shape. A future incompatible envelope shape gets a new version; readers reject unsupported versions. |
| Knownness | `status="known"` requires exactly one non-null `value_micros`, including when it is zero. `status="unknown"` requires `value_micros` to be absent. Unknown means this component has no usable result; it is not a numeric score or an error. |
| Numeric scale | Signed fixed point, where one unit is `1,000,000` micros. The envelope consumes an already formed signed integer micro-value; it does not round or convert fractional input. Component owners define any calculation or rounding before producing this value. |
| Range | `-9223372036854775808` through `9223372036854775807` micros inclusive (signed 64-bit range). |
| Invalid values | `value_micros` must match `0|-?[1-9][0-9]*` and be within range. Reject decimals, exponent notation, plus signs, leading zeros, and negative zero. Reject missing/null known values; null or present values for unknown; missing/unknown status; unsupported version; extra fields; and duplicate JSON object keys. Do not coerce invalid data to unknown or zero. |
| Overflow | A value outside signed 64-bit range is rejected at the envelope boundary. A component calculation that overflows must return an error before producing an envelope; never wrap, clamp, or saturate into a known result. |
| Determinism | The result uses exact integer micros and canonical integer spelling. The same component result and version therefore have the same represented value. Envelope semantics do not promise byte-for-byte JSON key ordering. |
| Errors | Invalid envelope data and component calculation failure are errors, not `unknown`. This envelope does not prescribe error codes or how callers report them. |

The language-neutral shape is a versioned tagged union: `Known { value_micros: i64 } | Unknown`. In JSON the tag is `status`; for `known`, `value_micros` is a canonical decimal string so clients that cannot exactly represent all signed 64-bit integers do not lose precision. Version 1 is closed: each variant has only the listed fields.

## Representation examples

```json
{"version":1,"status":"known","value_micros":"1250000"}
{"version":1,"status":"known","value_micros":"0"}
{"version":1,"status":"unknown"}
```

The first value represents exactly 1.25 units; the second is a known zero. Unknown remains distinguishable from both. These examples demonstrate encoding only and establish no scoring policy.

Invalid examples:

```json
{"version":1,"status":"known","value_micros":"1.25"}
{"version":1,"status":"known","value_micros":null}
{"version":1,"status":"unknown","value_micros":"0"}
{"version":1,"status":"known"}
{"version":1,"status":"known","value_micros":"-0"}
{"version":2,"status":"unknown"}
```

## Exclusions and qualification

This proposal does not define how components are calculated, rounded, combined, ordered, weighted, normalized, or tied; candidate or snapshot fields; unknown reasons; persisted routing-decision shape; APIs, migrations, SDKs, or runtime behavior. No `RoutingEngine`, `ScoreComponent`, or `RoutingDecision` implementation seam is present in the reviewed source.

Direct consumers **#191, #215, #217, #212, #213, #220, and #188 remain BLOCKED**. The owner must update each consumer brief to cite the exact accepted immutable revision of this artifact and its other applicable accepted inputs; state exact field/types, required/optional/null/unknown rules, errors and invariants, compatibility and migration/backfill/rollback behavior where applicable; provide meaningful fixtures and a runnable command; then re-size against qualified source seams before dispatch. Publication, merge, or issue readiness does not mean David/Cos accepted this proposal. For #220, preserve its existing prerequisites and qualify the component inputs before implementing combination behavior.

## Evidence basis

Reviewed against the immutable baseline `4472ca59896465fcf27b0d1df1d5218552d80efd`, live #549 brief, #186 policy/interface brief, #220 consumer handoff, [`../specs/2026-09-26-steve-gateway.md`](../specs/2026-09-26-steve-gateway.md) §§4, 8–9, [`../plans/2026-09-26-steve-mvp.md`](../plans/2026-09-26-steve-mvp.md) M6 and canonical scenario, and the source tree under [`../../src`](../../src). Runtime and mutation evidence: N/A for this documentation-only decision.
