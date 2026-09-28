# Provider transport contract index

This is the coordination index for [STV-PROV-02 #132](https://github.com/djh00t/steve/issues/132), not a replacement for its child contracts. The accepted #491 policy is recorded below; planned paths remain non-authoritative until their own producer revisions and executable gates are ready.

| Producer | Canonical artifact | Acceptance / consumer gate |
|---|---|---|
| [STV-PROV-38 #491](https://github.com/djh00t/steve/issues/491) | `docs/providers/tls-contract.md` | ACCEPTED via [PR #494](https://github.com/djh00t/steve/pull/494), exact revision `1cb584f137dc648ef98a726def5756b7b63b429c`, merge `624cf29e549ec01b062dcd25655ee083b6b8f6b9`; #495 is READY; #133 coordinates #602/#603/#604, with #602 READY and #603/#604 blocked; #134/#135 and #496–#498 remain blocked on their executable prerequisites. |
| [STV-PROV-39 #492](https://github.com/djh00t/steve/issues/492) | `docs/providers/credential-transport-contract.md` | BLOCKED on accepted #100; binds its account-owned credentials to protocol headers without redefining grants/storage/lifecycle. |

#100 remains the authority for Provider/UpstreamAccount schema and credential lifecycle. #398 retains both OpenAI and Anthropic account-sourced credential isolation; TLS-only work must not forward caller credentials. #133 coordinates #602/#603/#604; #602 owns reqwest/tokio-rustls dependency pins in `Cargo.toml`, #496 owns runtime client wiring, #603 owns the trusted fixture and cleanup through a caller-provided Axum `Router` because `src/test_upstream::router()` is binary-private, and #604 owns negative certificate variants. #495/#496 precede #134/#135, then #497/#498 qualify rejection with serialized test ownership. Dependency Advisor precedes any proposed direct TLS/signing dependency change.

#491 is accepted by its producer merge; #492 remains blocked on #100. Before closure, record exact accepted revisions for remaining children and update/re-size consumer briefs with owned paths, fixture inputs, meaningful qualification commands and mutation expectations. Future commands are not passing evidence. Keep #132 and provider parent #70 open until their full exits are proved.
