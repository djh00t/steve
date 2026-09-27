# STV-M2-60 proposal: principal IDs and relationships

**Status: proposal only.** David/Cos must accept this exact artifact revision before consumers treat its choices as binding. It changes no runtime behavior and authorizes no implementation. [#500 principal proposal](https://github.com/djh00t/steve/issues/500) is a child of the [#99 identity contract](https://github.com/djh00t/steve/issues/99); [#501](https://github.com/djh00t/steve/issues/501) owns initial principal/bootstrap credentials and [#502](https://github.com/djh00t/steve/issues/502) owns inference authentication and identity handoff.

## Record contract

Use `Organisation`, `User`, and `Client` records in `organisations`, `users`, and `clients` tables.

| Record | Required fields | Nullable field | Relationship |
|---|---|---|---|
| Organisation | `id`, `name`, `created_at` | `inactive_at` | Root |
| User | `id`, `organisation_id`, `name`, `created_at` | `inactive_at` | Exactly one Organisation |
| Client | `id`, `user_id`, `name`, `created_at` | `inactive_at` | Exactly one User |

| Field | Contract and write owner |
|---|---|
| `id` | Rust `Uuid`, immutable UUIDv7 primary key; #124 store generates it with `Uuid::now_v7()`. PostgreSQL column `UUID`; SQLite column canonical lowercase hyphenated UUID `TEXT`. |
| `organisation_id`, `user_id` | Required immutable principal foreign keys; PostgreSQL `UUID`, SQLite canonical UUID `TEXT`; `ON DELETE RESTRICT`. #105/#107 declare and verify the constraints. |
| `name` | Required `TEXT` / Rust `String`; #124 trims surrounding whitespace and rejects an empty result. No uniqueness or length rule is proposed. |
| `created_at` | Required immutable UTC RFC3339 `TEXT` on both backends; #124 sets it at creation and callers cannot supply it. |
| `inactive_at` | Nullable UTC RFC3339 `TEXT`; `NULL` means not inactivated. #124 sets it once on disable; callers cannot set or clear it. |

Required columns are `NOT NULL`; `id` is the primary key; parent columns are `NOT NULL` foreign keys with `ON DELETE RESTRICT`. Do not add cascade deletion.
At the write boundary #124 rejects `name.trim().is_empty()` and stores the trimmed value; #105/#107 add `CHECK (name <> '')` as a storage guard.
The #124 store generates both timestamps with `Utc::now().to_rfc3339()`; persisted values remain UTC RFC3339 strings, never local time.

UUIDv7 uses the existing `uuid` dependency and current `Uuid` request/attempt ID types. It sorts by approximate creation time, so IDs are references only, never secrets or proof of authority. [Cargo.toml](../../Cargo.toml) [src/proxy/types.rs](../../src/proxy/types.rs) [RFC 9562 §5.7](https://www.rfc-editor.org/rfc/rfc9562.html#name-uuid-version-7)

## Relationships, inactivation, and attribution

- Each User has one Organisation parent; each Client has one User parent. Client covers an app, IDE, CLI, agent, service account, or automation. This matches the spec’s Organisation → User → Client attribution chain. [Spec §5](../specs/2026-09-26-steve-gateway.md#5-identity-and-attribution)
- A Client can be freshly resolved only when it and both ancestors have `inactive_at IS NULL`. Disabling a parent does not update descendants’ timestamps; any inactive ancestor makes the chain ineligible for fresh resolution.
- Ordinary removal is disable/inactivation, never cascading physical deletion or reassignment to another parent. The rows and IDs remain readable for historical attribution. Physical purge is allowed only after no principal or retained attribution reference remains and the applicable retention policy permits it; M3 owns retention timing. [MVP plan](../plans/2026-09-26-steve-mvp.md)
- A request snapshots one complete tuple or none using `Option<PrincipalAttribution>` on both `Request` and `RequestAttempt`; it contains required UUIDs `organisation_id`, `user_id`, and `client_id`. Serialization, legacy, and unknown-member rules are defined below.
- New successful Client resolution derives all three IDs from stored parent links; it never accepts caller-supplied ancestry. Attempts, retries, and clones preserve the request’s captured tuple unchanged, even if a principal is later disabled. In-flight cancellation/revocation semantics stay with #501/#502/#100.
- No synthetic Organisation, User, or Client is created for an unattributable request. Unauthenticated-request handling and inference wire outcomes remain [#502](https://github.com/djh00t/steve/issues/502) decisions.
- Principal IDs are not credentials. Bootstrap principals and client credentials stay with [#501](https://github.com/djh00t/steve/issues/501); inference authentication and resolved-identity handoff stay with [#502](https://github.com/djh00t/steve/issues/502). Grants and provider-account authorization stay with [#100](https://github.com/djh00t/steve/issues/100); local identity/authentication boundaries remain under [#99](https://github.com/djh00t/steve/issues/99). Management authorization, audit events, and Session/Turn association remain outside #500.

### Attribution wire rules

For both `Request` and `RequestAttempt`, newly serialized values must contain the `attribution` member. Its value is either JSON `null` or an object with exactly the three required, non-null members `organisation_id`, `user_id`, and `client_id`, each a canonical UUID string. This all-or-null choice has no representation for partial ancestry. On legacy deserialization only, an absent member and explicit `null` both mean unknown attribution and map to `None`; a present object with a missing, null, or malformed ID is rejected. Unknown members inside the attribution object are rejected; this proposal does not alter outer `Request`/`RequestAttempt` unknown-member handling. The implementation consumer must encode the legacy absent-member rule explicitly rather than infer it from current Serde defaults.

### Documentation fixtures (not executable evidence)

All IDs and timestamps below are synthetic contract examples, not observed runtime values. Complete attribution is identical on the logical request and its attempt; legacy absence/null means no trusted tuple; partial attribution and a missing parent are rejected.

```json
{
  "complete_request": {
    "id": "00000000-0000-7000-8000-000000000001",
    "created_at": "2026-01-01T00:00:00Z",
    "attempts": ["00000000-0000-7000-8000-000000000002"],
    "attribution": {
      "organisation_id": "00000000-0000-7000-8000-000000000003",
      "user_id": "00000000-0000-7000-8000-000000000004",
      "client_id": "00000000-0000-7000-8000-000000000005"
    }
  },
  "complete_attempt": {
    "id": "00000000-0000-7000-8000-000000000002",
    "request_id": "00000000-0000-7000-8000-000000000001",
    "provider": "example-provider",
    "account": "example-account",
    "status": "success",
    "started_at": "2026-01-01T00:00:00Z",
    "finished_at": "2026-01-01T00:00:01Z",
    "attribution": {
      "organisation_id": "00000000-0000-7000-8000-000000000003",
      "user_id": "00000000-0000-7000-8000-000000000004",
      "client_id": "00000000-0000-7000-8000-000000000005"
    }
  },
  "legacy_request_attribution_absent": {
    "id": "00000000-0000-7000-8000-000000000006",
    "created_at": "2026-01-01T00:00:00Z",
    "attempts": []
  },
  "legacy_request_attribution_null": {
    "id": "00000000-0000-7000-8000-000000000007",
    "created_at": "2026-01-01T00:00:00Z",
    "attempts": [],
    "attribution": null
  },
  "reject_partial_attribution": {
    "attribution": {
      "organisation_id": "00000000-0000-7000-8000-000000000003",
      "user_id": "00000000-0000-7000-8000-000000000004",
      "client_id": null
    },
    "expected": "reject"
  },
  "reject_user_with_missing_organisation": {
    "user_row": {
      "id": "00000000-0000-7000-8000-000000000008",
      "organisation_id": "00000000-0000-7000-8000-000000000009",
      "name": "Example user",
      "created_at": "2026-01-01T00:00:00Z",
      "inactive_at": null
    },
    "existing_organisation_ids": [],
    "expected": "foreign_key_rejection"
  }
}
```

These are illustrative documentation fixtures only. Before contract acceptance, the registry requires an executable fixture and its exact runnable command/evidence at the reviewed revision. No command result or runtime qualification is claimed here.

## Compatibility and migration scope

- At source `4472ca59896465fcf27b0d1df1d5218552d80efd`, storage supports SQLite and PostgreSQL; the v1 migrations define the migration ledger and `steve_background_events`. `Request` and `RequestAttempt` are logical Serde types, not persisted request tables. Leave v1 rows and event payloads unchanged. [src/storage/db.rs](../../src/storage/db.rs) [src/proxy/types.rs](../../src/proxy/types.rs)
- Add the three principal tables and required FKs as additive migrations for each backend. Follow migration contract #102 and the SQLite/PostgreSQL runners #103/#104; this proposal does not claim those contracts/runners are accepted or specify their transaction mechanics. [#102](https://github.com/djh00t/steve/issues/102) [#103](https://github.com/djh00t/steve/issues/103) [#104](https://github.com/djh00t/steve/issues/104)
- The inspected v1 migration definitions provide no attribution-bearing persisted request rows to backfill. For legacy serialized Request/RequestAttempt values, missing or null `attribution` means unknown. If future persisted data has a proven mapping, backfill only the complete verified tuple; otherwise preserve unknown as null.
- Rollback does not drop identity tables or captured references. Stop applying later migrations. An older binary may run only after its migration and runtime compatibility with the newer schema has been verified; the migration contract rejects unknown schema versions. Otherwise require operator recovery; permanent downgrade/purge waits until the approved retention rule allows it.
- SQLite must enforce the declared foreign keys; the implementation consumer must include a missing-parent rejection case. Existing SQLx 0.8.6 SQLite options enable foreign keys by default, so do not add a connection workaround unless a focused check fails. [#105](https://github.com/djh00t/steve/issues/105) [SQLx 0.8.6 SQLite options](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/options/mod.rs#L185)

## Consumer handoff and review decisions

David/Cos acceptance and merge of this exact proposal revision accepts only the principal ID, relationship, and attribution choices above. It does not implement or accept database migrations, authentication, management, or inference behavior, and does not waive any downstream consumer gates.

- #105 owns SQLite principal tables/FKs; #107 owns PostgreSQL principal tables/FKs; #118 owns Request/RequestAttempt attribution and serialization; #124 owns principal metadata create/read/update/disable. These consumers stay blocked until #99’s identity/auth contract composes with #106 and each consumer is resized against this proposal’s accepted revision. See the [M2 backlog index](../backlog-index.md) and [contract index](README.md).
- Re-size #124 before dispatch: its current acceptance mixes client-key issue/rotation with principal CRUD despite the out-of-scope text. Keep key issue/rotation/revocation in #129/#130; retain #124’s existing admin/audit prerequisites while changing only its principal acceptance case.
- Review decision: the spec shows one Organisation → User → Client chain but does not state whether a User may belong to multiple Organisations. This proposal chooses exactly one; if MVP requires multi-organisation membership, revise this contract before acceptance and consumer dispatch. [Spec §5](../specs/2026-09-26-steve-gateway.md#5-identity-and-attribution)
- Purge timing after the no-reference condition remains an explicit M3 retention decision. Shared-account access stays with #100; bootstrap and request authentication stay with #501/#502.

## Negative cases

- Reject a User without an existing Organisation or a Client without an existing User.
- Reject a Client attribution object missing any of its three IDs; reject ancestry inconsistent with stored parent links.
- Reject physical deletion while principal or retained attribution references exist; disabling preserves historical identity snapshots.
- Do not backfill ownership from provider/account names, client-supplied labels, configuration, IP address, or event payload without a verified mapping.
