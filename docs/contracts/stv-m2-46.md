# STV-M2-46: audit contract work packages

This is a coordination index, not an accepted audit schema or durability policy. [#110](https://github.com/djh00t/steve/issues/110) remains open until the complete original audit contract is explicitly accepted by David or Cos. Detailed briefs live in the child issues.

| Child | Owned proposal artifact | Dispatch boundary |
| --- | --- | --- |
| [#513: envelope and redaction](https://github.com/djh00t/steve/issues/513) | `stv-m2-46-envelope.md` | READY for proposal only; exact identity bindings required before acceptance |
| [#514: operations and attribution](https://github.com/djh00t/steve/issues/514) | `stv-m2-46-inventory.md` | READY for proposal only; exact bindings and acceptance wait for accepted envelope/identity decisions |
| [#515: durability and failure behavior](https://github.com/djh00t/steve/issues/515) | `stv-m2-46-durability.md` | BLOCKED until accepted accounting #483/#484/#485 revisions |

These are planned artifact paths, not existing or accepted contracts. Each child targets five to ten active minutes after its prerequisites. Children own separate files; the integrator alone updates this index and the registry. No runtime, migration or shared writer edit belongs to a proposal child.

## Evidence and preserved scope

Spec sections 5, 14 and 17 require attribution, security/configuration audit events and no silent loss of authoritative audit data. `src/deferred.rs::AccountingEvent` and `src/storage/db.rs::DatabasePool::insert_background_event` supply a generic event envelope and idempotent background insert; they do not establish an audit schema, audit producers or durable acknowledgement. The journal currently flushes lines; this is not proof of crash durability.

The integrated contract must cover identity/client-key, provider credential, account binding/grant, pool and history-policy changes, and explicitly handle static configuration with no runtime actor. Every operation needs redacted actor/target/outcome and attribution, event ordering, acknowledgement and failure rules. A management state change's success when audit persistence fails is a separate decision from preserving an in-flight inference request; do not infer one from the other.

## Acceptance and consumer handoff

David or Cos must explicitly accept exact child revisions. Proposal publication, merge and CI do not establish policy acceptance. The integrator reconciles all three artifacts against their accepted identity and accounting dependencies before closing #110. Parent #63 stays open for its remaining work.

Shared audit writer [#123](https://github.com/djh00t/steve/issues/123) stays BLOCKED and retains its storage, migration and test prerequisites. Re-size it and per-operation producers against accepted artifacts and available source seams before READY; supply an executable fixture and command for each runtime slice. Planned test names and zero selected tests are not evidence. These documentation packages use source/contract review, without artificial runtime or mutation tests.
