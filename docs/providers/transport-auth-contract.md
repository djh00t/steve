# Provider transport contract index

This is the coordination index for [STV-PROV-02 #132](https://github.com/djh00t/steve/issues/132), not an accepted security or transport contract. Planned paths are not implementation-ready inputs.

| Producer | Canonical artifact | Acceptance / consumer gate |
|---|---|---|
| [STV-PROV-38 #491](https://github.com/djh00t/steve/issues/491) | `docs/providers/tls-contract.md` | READY to propose TLS backend/trust and deterministic fixture input; David/Cos accepts exact revision before #133/#134/#135 dispatch. |
| [STV-PROV-39 #492](https://github.com/djh00t/steve/issues/492) | `docs/providers/credential-transport-contract.md` | BLOCKED on accepted #100; binds its account-owned credentials to protocol headers without redefining grants/storage/lifecycle. |

#100 remains the authority for Provider/UpstreamAccount schema and credential lifecycle. #398 retains both OpenAI and Anthropic account-sourced credential isolation; TLS-only work must not forward caller credentials. #133 owns reqwest integration; #495/#496 precede #134/#135, then #497/#498 qualify rejection with serialized test ownership. Dependency Advisor precedes any proposed direct TLS/signing dependency change.

No child revision is accepted by this index. Before closure, record exact accepted revisions and update/re-size consumer briefs with owned paths, fixture inputs, meaningful qualification commands and mutation expectations. Future commands are not passing evidence. Keep #132 and provider parent #70 open until their full exits are proved.
