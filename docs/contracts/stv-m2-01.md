# Local identity and client authentication contract index

[STV-M2-01 #99](https://github.com/djh00t/steve/issues/99) coordinates the three decisions below. This index is not an accepted identity or authentication contract; planned paths are not implementation-ready inputs.

| Producer | Canonical artifact | Acceptance gate |
|---|---|---|
| [STV-M2-60 principal IDs and relationships](https://github.com/djh00t/steve/issues/500) | `docs/contracts/stv-m2-60.md` | PROPOSED in merged [PR #518](https://github.com/djh00t/steve/pull/518) at `504dabec31f8fa086c43667b3da173e02f3f5617`; explicit David/Cos acceptance and executable contract evidence remain pending. |
| [STV-M2-61 bootstrap and client credential lifecycle](https://github.com/djh00t/steve/issues/501) | `docs/contracts/stv-m2-61.md` | BLOCKED on accepted STV-M2-60 and management/listener contract #106. |
| [STV-M2-62 inference auth and resolved identity](https://github.com/djh00t/steve/issues/502) | `docs/contracts/stv-m2-62.md` | BLOCKED on accepted STV-M2-60/STV-M2-61. |

Principal field types, relationships, null/unknown and legacy attribution belong to STV-M2-60. Bootstrap and inference client credential issuer/lifecycle/storage decisions belong to STV-M2-61. Inference headers, parsing, route coverage, identity handoff and wire errors belong to STV-M2-62. STV-M2-61 owns issuance, storage, retrieval, and secret-disclosure invariants; STV-M2-62 consumes those invariants and owns request-time header/log/trace/error redaction. Credential state definitions come from STV-M2-61; STV-M2-62 maps them to request outcomes.

Provider accounts, upstream credentials and access grants remain in #100. Listener defaults, remote TLS and management authentication remain in #106. Security audit schema belongs to #110; Session/Turn association belongs to #116. STV-M2-62 passes authenticated identity to the account access boundary; it must not invent a second grant policy. No incoming client credential becomes an upstream provider credential.

#106 has no new prerequisite on #99 or the bootstrap child. The bootstrap child consumes its accepted exposure/auth boundary. If #106 needs shared principal field types, it may consume STV-M2-60 independently without depending on bootstrap or inference auth.

Keep every existing #99 consumer behind that gate until all three exact child revisions are accepted, their interfaces compose with #106, and each consumer is re-sized against the accepted fields/errors and available code/fixtures. Record accepted revisions here before closing #99. M2 epic #63 remains open until its complete acceptance is proved.

Source at `2f383521b37ac1785b711397740c186f25d62e7a` has no Organisation/User/Client schema or inference auth middleware. Both default listeners use wildcard addresses, whereas spec section 14 requires local loopback defaults. That existing mismatch is recorded for #106; this planning split changes no runtime security behavior.
