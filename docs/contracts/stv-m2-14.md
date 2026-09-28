# STV-M2-14 #106: management and listener security composition

**State: composed from accepted child artifacts.** The listener/TLS and management-auth boundaries are authoritative in the exact merged revisions below. This parent records composition and downstream gates; it does not duplicate or re-accept their policy. #590 synchronization merged via [PR #596](https://github.com/djh00t/steve/pull/596) at head `efa4a6b275673b0fe99b330ad827b19b085171f2` (merge `3e18e0f932617e982e8c8e67409ab2cbe79c34a7`). #106 remains open until a TLS implementation/qualification owner is named and the #109/#143 briefs are re-sized; runtime implementation and testing remain downstream and out of #106 scope.

## Canonical child artifacts

| Boundary | Canonical artifact | Accepted revision | Acceptance provenance |
| --- | --- | --- | --- |
| Listener defaults, remote TLS, downgrade and forwarded-header rules | [STV-M2-14A #576](https://github.com/djh00t/steve/blob/5d65e513bbc4d50c2585ea7387f2aee8d4eb7a98/docs/contracts/stv-m2-14a-listener-tls.md) | `5d65e513bbc4d50c2585ea7387f2aee8d4eb7a98` | David/Cos review-and-merge; [PR #588](https://github.com/djh00t/steve/pull/588) merged as `55c2ba093784d186eb98cf1967206f4f514884d5` |
| Management bearer authentication and credential lifecycle | [STV-M2-14B #577](https://github.com/djh00t/steve/blob/8123b75fe53831db32d17bd1167422f7688bb1cc/docs/contracts/stv-m2-14b-management-auth.md) | `8123b75fe53831db32d17bd1167422f7688bb1cc` | David/Cos review-and-merge; [PR #589](https://github.com/djh00t/steve/pull/589) merged as `37246d359ff38ef13f56cbb9fa1abdeaeb5a704a` |

Those exact child revisions are the single source of truth for A/B/C policy details, startup validation, request outcomes, redaction, rotation, rollback, and qualification requirements. A change to either boundary requires a new child revision and a fresh composition update.

## Implementation gates

- **#106 remains open.** #590 synchronization has merged; closure still requires a TLS implementation/qualification owner and re-sized #109/#143 briefs. Runtime implementation and testing are downstream and out of #106 scope.
- **#109 remains blocked.** Re-size management implementation against STV-M2-14B and its full route-wide authentication requirement, with identity and listener/TLS prerequisites still enforced.
- **#143 remains blocked.** Re-size listener/TLS qualification against STV-M2-14A. A TLS implementation and qualification owner is still missing; no runtime TLS evidence is claimed here.
- Remote inference requires the [#502](https://github.com/djh00t/steve/issues/502) inference-authentication boundary to be accepted and implemented, as specified by STV-M2-14A.

This documentation composition does not claim implementation readiness, deployment qualification, or completion of #106 or its downstream issues.
