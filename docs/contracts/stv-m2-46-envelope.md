# STV-M2-46A (#513): audit envelope and redaction proposal

**Status:** READY for proposal review only. This document proposes policy; it does not accept the contract or authorize runtime work.

**Proposed repository path:** `docs/contracts/stv-m2-46-envelope.md`  
**Acceptance record:** Pending David or Cos explicitly accepting this exact artifact revision. Record the artifact path, immutable revision/commit, and accepter at #110. #513 and #514 may be accepted jointly by recording both exact revisions; neither requires prior acceptance of the other. No revision or acceptance is claimed here.

## Scope and source basis

The envelope covers one security-relevant operation result or decision per event. It provides stable event identity, operation-time timestamp, actor, target, outcome, and attribution. It does not define the operation inventory (#514), storage durability, or mutation-versus-audit failure behavior (#515); those remain separate decisions under #110.

The gateway specification requires request attribution through Organisation → User → Client → Session → Turn → Request → RequestAttempt wherever available (§5), audit events for configuration/security changes (§14), and requires authoritative accounting/audit data not be silently lost while keeping deferred work off the request hot path (§17). The current `AccountingEvent` is `{ id: String, kind: String, payload: Value, created_at: String }`; its writer inserts that arbitrary payload into `steve_background_events`. It is not an audit-specific schema or producer. [Principal IDs producer #500](https://github.com/djh00t/steve/issues/500) is open; no accepted principal artifact is supplied by this PR. This proposal relies on the published issue as a dependency, not on an unpublished local draft.

## Proposed v1 envelope

The wire format is JSON. Every member below is required. Nullable members must still be present. No arbitrary payload or metadata member exists.

| Field | Exact JSON type | Required/null rule | Proposed meaning and validation |
|---|---|---|---|
| `schema_version` | integer | Required; non-null | Exactly `1` for this schema. |
| `event_id` | string | Required; non-null | Canonical lowercase, hyphenated UUIDv7 generated once when the logical event is created. Queue retries and journal replay retain it unchanged; a new logical event gets a new ID. It is an identifier, not an authorization secret. |
| `occurred_at` | string | Required; non-null | UTC RFC3339 timestamp at the operation-result boundary: when success, denial, or failure is determined, before enqueue/journal handoff. It is not the later database insert time. |
| `actor` | object | Required; non-null | Exactly `{kind, id}`. `kind` is one of `user`, `client`, `management_admin`, `system`, `anonymous`, `unknown`. `id` is a non-empty string for `user`/`client`, otherwise JSON `null`. Successful authentication with the shared #106 `management.admin` bearer role uses `management_admin` with `id: null`; it identifies the role, never a person. Failed authentication uses `anonymous` with `id: null`. No display name, email, credential, or caller-supplied identity label. |
| `action` | string | Required; non-null | Stable lower-snake-case operation name, registered by the accepted #514 inventory. Dynamic values and unregistered names are invalid. |
| `target` | object | Required; non-null | Exactly `{kind, id}`. `kind` is a lower-snake-case target kind registered by #514, or `none` / `unknown`. `id` is a non-empty stable identifier for a registered concrete kind, otherwise JSON `null`. Never a credential or secret value. |
| `outcome` | string | Required; non-null | Closed enum: `success`, `denied`, or `failure`. The event records a final operation result, not an in-progress state. |
| `error_code` | string or null | Required; nullable | `null` for `success`; a stable lower-snake-case code for `denied` or `failure`. Codes are registered in #514; `unclassified_failure` is the required safe fallback when failure is known but its detail is not classifiable. Free-form messages are forbidden. |
| `attribution` | object | Required; non-null | Exactly the seven dimension references below, all present. |

Each attribution dimension is exactly `{state, id}`. `state` is a closed enum: `known`, `unknown`, or `not_applicable`. `id` is a non-empty string iff state is `known`; otherwise it is JSON `null`.

| Dimension | Represents |
|---|---|
| `organisation` | Organisation in the request’s attribution chain |
| `user` | User in the request’s attribution chain |
| `client` | Client in the request’s attribution chain |
| `session` | Steve session |
| `turn` | Turn within the session |
| `request` | Logical request |
| `attempt` | One upstream request attempt |

`unknown` means the dimension applies or may apply but no trusted identifier is available. `not_applicable` means the operation has no such dimension. A missing dimension object or missing member is invalid; it never means unknown or not applicable. Partial known ancestry is permitted to preserve what is available and must not be completed by inference. A known reference is emitted only from a trusted producer-side source, never from a client label or guessed relationship.

For an operation authenticated only by the shared management-admin role, set Organisation, User, and Client to `unknown`; the credential authenticates the role and supplies no person or organisation identity. Set Session, Turn, Request, and Attempt to `not_applicable` when there is no independently trusted context for those dimensions. If an independently trusted source supplies a dimension, preserve that reference instead of applying this baseline. A failed authentication has actor `anonymous` and no trusted identity; use the same unknown/not-applicable attribution baseline absent independently trusted context, and do not copy claims from the failed request into attribution.

### ID binding gate

All reference IDs use JSON strings on the wire. For UUID-backed IDs, the proposed encoding is canonical lowercase, hyphenated UUID text; source currently uses UUID for Request/Attempt IDs and creates the generic accounting event ID with UUIDv7. Before accepting this contract pair, bind `actor.id`, concrete `target.id`, and each `attribution.*.id` to the accepted identity and entity contracts, including which dimensions are UUID-backed and the canonical form for Session/Turn IDs. In particular, do not infer Organisation/User/Client acceptance from [#500](https://github.com/djh00t/steve/issues/500); that producer remains open. If the accepted contracts do not supply a stable safe identifier for a dimension, that dimension remains `unknown`, never a fabricated or display identifier.

### Unknowns, compatibility, and error behavior

- Version 1 rejects unknown fields, missing required members, invalid null/state combinations, unknown enum values, malformed IDs/timestamps, unregistered actions/targets/error codes, and unsupported schema versions. It does not coerce or silently discard malformed required data.
- Any field, enum, identifier encoding, or semantic change requires an explicitly reviewed schema version. Readers reject a version they do not support; writers must not silently downgrade or reinterpret an event. An older binary may only run if its compatibility with persisted newer events is separately verified.
- Invalid event construction returns an audit-validation error and emits no event. This does not by itself choose whether a related configuration/state mutation is committed, rolled back, or rejected; #515 owns that coupling and durability policy. Envelope signoff does not accept #515 or resolve that behavior. It also does not waive §17’s durability requirement.
- Adding a versioned envelope does not make current `AccountingEvent` payloads conformant, and does not change or migrate existing `steve_background_events` rows. A later storage/migration consumer must be sized against the accepted #513, #514, and #515 decisions. Rollback preserves already stored v1 rows and accepted references; it must not reinterpret arbitrary legacy accounting payloads as audit events.

## Redaction boundary

Use default-deny construction from the fields above. V1 admits no operation-specific metadata. Adding any such field requires a reviewed change to both envelope and inventory; no version/change reference or other extra value is implicitly allowed. There is no source-object serialization, arbitrary `payload`, generic `details`, error message, or extensible metadata map. The only values copied into an event are schema-validated identifiers from trusted identity/entity sources and registered action/target/outcome/error codes.

Never serialize, at any nesting depth:

- credentials, authorization headers, API keys, access/refresh tokens, cookies, private keys, verifier material, or secret values;
- prompt, response, tool, or other user-supplied content;
- raw upstream request/response bodies, raw exception/error strings, or unclassified input;
- names, email addresses, or other display/PII fields not explicitly admitted as safe by a later accepted contract.

Omit prohibited or unclassifiable optional source values; never replace them with a redaction marker copied into the event. If omission leaves a required envelope field invalid, return the audit-validation error above and do not emit a partial event. Actor/attribution `unknown` is valid only when the identifier itself is unavailable; it must not be used to conceal a value that was observed but could not be safely classified. A known identifier is allowed only after its cross-contract binding establishes it is a safe reference.

### Contract fixtures (not runtime evidence)

The following JSON Schema fragment is the shape fixture: it specifies wire names and types and contains no event values. The prose table above supplies validation and cross-field constraints not encoded in this deliberately partial shape fixture. It is not a complete executable validator.

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": [
    "schema_version", "event_id", "occurred_at", "actor", "action",
    "target", "outcome", "error_code", "attribution"
  ],
  "properties": {
    "schema_version": { "type": "integer" },
    "event_id": { "type": "string" },
    "occurred_at": { "type": "string" },
    "actor": {
      "type": "object", "additionalProperties": false,
      "required": ["kind", "id"],
      "properties": {
        "kind": { "type": "string", "enum": ["user", "client", "management_admin", "system", "anonymous", "unknown"] },
        "id": { "type": ["string", "null"] }
      }
    },
    "action": { "type": "string" },
    "target": {
      "type": "object", "additionalProperties": false,
      "required": ["kind", "id"],
      "properties": { "kind": { "type": "string" }, "id": { "type": ["string", "null"] } }
    },
    "outcome": { "type": "string", "enum": ["success", "denied", "failure"] },
    "error_code": { "type": ["string", "null"] },
    "attribution": {
      "type": "object", "additionalProperties": false,
      "required": ["organisation", "user", "client", "session", "turn", "request", "attempt"],
      "properties": {
        "organisation": { "$ref": "#/$defs/reference" },
        "user": { "$ref": "#/$defs/reference" },
        "client": { "$ref": "#/$defs/reference" },
        "session": { "$ref": "#/$defs/reference" },
        "turn": { "$ref": "#/$defs/reference" },
        "request": { "$ref": "#/$defs/reference" },
        "attempt": { "$ref": "#/$defs/reference" }
      }
    }
  },
  "$defs": {
    "reference": {
      "type": "object", "additionalProperties": false,
      "required": ["state", "id"],
      "properties": {
        "state": { "type": "string", "enum": ["known", "unknown", "not_applicable"] },
        "id": { "type": ["string", "null"] }
      }
    }
  }
}
```

The redaction fixture is a class-based contract, not a fabricated event or runtime observation:

```json
{
  "fixture_kind": "redaction_contract_cases",
  "cases": [
    { "source_class": "credential_or_secret", "expected": "omit", "serialized_source_value": false },
    { "source_class": "prompt_response_or_tool_content", "expected": "omit", "serialized_source_value": false },
    { "source_class": "raw_error_or_unclassified_input", "expected": "omit_or_safe_error_code", "serialized_source_value": false },
    { "source_class": "unclassifiable_required_value", "expected": "reject_event", "serialized_source_value": false }
  ]
}
```

| Source input class | Envelope result |
|---|---|
| Credential, token, key material, cookie, or secret value, including nested values | Omit; no raw or masked value is serialized. |
| Prompt, response, tool content, or arbitrary user input | Omit; no raw or summarized content is serialized. |
| Raw upstream body, exception text, or unclassified error detail | Omit; use only an already registered safe `error_code`, otherwise `unclassified_failure` when failure is known. |
| Name, email, display label, or caller-supplied identity | Omit; emit a trusted stable identifier only if the identity binding permits it, otherwise use the appropriate `unknown` actor/attribution state. |
| Unclassifiable value needed for a required field | Reject event construction; emit no partial/raw event and leave mutation coupling to its explicit accepted operation policy. |

These fixtures state the proposed contract only. They are not evidence that the current runtime validates, redacts, persists, or durably acknowledges audit events.

## Acceptance and implementation dependencies

David or Cos may review this schema separately and accept it jointly with #514 after the identity/entity bindings below are resolved. #110 records both exact artifact revisions; neither child requires prior acceptance of the other. Acceptance of this envelope does not accept #110 as a whole, establish runtime behavior, or authorize implementation.

Before contract acceptance, bind every actor, target, and attribution ID to the accepted identity/entity contract; do not infer Organisation/User/Client acceptance from the open [#500 producer](https://github.com/djh00t/steve/issues/500). Each producer action, target kind, and error code must also map to the accepted #514 inventory. Those source and vocabulary bindings gate contract acceptance and producer implementation; the current proposal does not claim they are resolved.

#515 separately owns durability and mutation-versus-audit failure behavior. Envelope signoff is separable from #515; #110 integration/closure and runtime dispatch remain gated on accepted durability/coupling and the other required child decisions. Any future implementation consumer must be re-sized against the accepted artifact revisions and receive the executable fixture and runnable command before dispatch.

Contract acceptance is also blocked until the producing contract packages supply a complete executable fixture and exact runnable command covering cross-field ID/null/state rules, closed vocabulary, unsupported versions and redaction rejection, with observed passing results on the named candidate. The partial shape and symbolic examples above do not satisfy this gate. Keep qualification bounded and reusable by runtime consumers; do not create tests that merely check prose text. No such executable contract qualification is claimed in this proposal.
