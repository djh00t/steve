# STV-M0-17: accounting journal ownership proposal

**Status: recommended proposal for David/Cos review; not accepted.** This revision was checked against main `675533dbe0e2d00af9ec71ca59eb34d9151ba63f`. It proposes the ownership and replacement boundary for #484 only. It does not accept an implementation, a platform guarantee, a #483 incident policy, or a #485 replay/completion rule.

## Recommended proposal

Give every replica one private, persistent, absolute journal root. A root belongs to one host and one local filesystem. Replicas may use the same deployment template, but their resolved roots must be different. The root contains:

- a persisted installation identity and coordination lock;
- the replica-scoped incident evidence owned by the #483 contract; and
- one journal and state record per process generation.

The `queues.accounting_journal` value must resolve to an absolute path before startup. The current relative default (`data/accounting-overflow.jsonl`) has no stable meaning across working directories and is not an acceptable adopted value. The implementation should reject a relative value visibly rather than silently choosing a working directory. The resolved path and installation identity must remain stable across process generations.

If the configured root is absent, no longer resolves to the persisted root, is on an unsupported filesystem, or lacks expected ownership or incident evidence, startup fails closed. A later process must not recreate a missing record as healthy, replay an unknown file, or truncate anything to make readiness pass. Moving a replica to another host is supported only when the same root and qualified filesystem are still available; cross-host orphan discovery and recovery are out of scope. An explicit offline migration is required when they are not.

The proposal is deliberately separate from the sibling contracts:

- #483 chooses incident state names, admission ordering, restart lifetime, readiness/status responses, and operator disposition. #484 supplies durable ownership and revision-checked evidence to that policy; it does not choose those transitions.
- #485 chooses drain, replay acknowledgement, completion evidence, retry interaction, and retirement ordering. #484 never treats a database `Ok`, channel send, flush, or released OS lock as replay completion.

## Generation ownership and replacement

For each generation, create a private staging file, acquire its exclusive OS lock, and publish it atomically under a generation UUID only after the lock is held. Write the corresponding `active` state and sync the required file and directory before readiness. Append only to the published file while holding its lock. A scanner uses a nonblocking lock attempt; a busy file belongs to its writer or replayer and is not read, truncated, or retired.

The handoff is:

1. The new process resolves and verifies the private root, installation identity, coordination lock, incident evidence, and its own generation staging file.
2. It publishes and syncs its active generation, then takes a complete evidence snapshot and may become ready while the healthy predecessor remains alive.
3. The predecessor drains under #485 and releases its generation lock only after #485's accepted completion evidence is durable.
4. A successor may inspect an unlocked predecessor file only after checking its generation state and the current incident evidence under the coordination lock. Missing, corrupt, non-clean, or changed evidence is reported to #483 as unresolved; an empty journal does not prove that volatile work was absent.
5. Replay and retirement occur only under #485's accepted acknowledgement and retirement ordering. A live journal is never truncated.

The coordination record uses a monotonic revision. Reads and read/modify/write updates take the coordination lock and reread the current revision; a stale clear, acknowledgement, or evidence update cannot overwrite a newer failure. The lock protects serialization only. It does not itself prove a clean exit, a durable append, or an incident transition.

The proposed timing values below are policy inputs for review, not guarantees from this document:

- poll complete incident and generation evidence every **50 ms**;
- consider the in-memory snapshot fresh for **100 ms** from poll start; and
- use **100 ms** as the proposed coordination-lock acquisition timeout.

An incomplete or expired snapshot and every lock, read, write, sync, or atomic-publication error fail closed. No observation bound is claimed when a process is unscheduled, when publication has not succeeded, or before the named platform qualification exists.

## First adoption of the legacy flat file

The current flat file is written and replayed without ownership coordination. It cannot overlap with the new protocol. Its first adoption is a one-time offline maintenance operation, not an automatic startup migration:

1. Stop every pre-protocol writer and record the maintenance boundary. Do not proceed without accepted evidence that no old binary can append.
2. Preserve the original bytes as an immutable backup. Record its byte length and hash, sync the backup, and sync its containing directory. Leave the original untouched.
3. Under the new coordination lock, create and lock a staging generation. Parse the backup; any framing or torn-tail decision remains with #85. Write the migrated records to the staging generation, sync it and its directory, and retain the original and backup.
4. Reconcile records under #485's replay and acknowledgement contract. Write an adoption marker binding the source hash and length, target generation ID, record outcome, and accepted evidence reference; sync the marker and its directory.
5. Only after that marker is durable, atomically move the legacy file out of active discovery, retain it for recovery, and sync the directory. Publish the adopted generation and permit normal rolling overlap.

A crash before the adoption marker leaves the source and backup authoritative; startup fails closed and the operation resumes from the backup. A crash after the marker but before the final directory sync also fails closed until the marker, source hash, target generation, and directory evidence are reconciled. There is no auto-truncate, delete, or silent `clear`. Accepted migration evidence is the stopped-writer record, source hash/length, durable backup, parser outcome, #485 replay acknowledgement, target generation ID, adoption marker, and final directory sync. If any item is absent, the migration is incomplete.

## Platform and durability boundary

No OS/filesystem pair is supported by this proposal. Qualification must pass separately for every deployment pair, with the baseline named explicitly as Linux/ext4, macOS/APFS, and Windows/NTFS. Each pair must demonstrate exclusive lock behavior, lock retention across staging publication, concurrent append visibility, file and directory sync ordering, atomic replacement, and lock release after process death. A deployment using another filesystem needs its own result before it is supported.

CI can exercise the protocol and failure ordering; CI alone cannot prove any power-loss guarantee. Power loss during append, database commit, publication, or retirement remains unknown until separately qualified with the actual OS, filesystem, and storage path. Unsupported lock or sync semantics must reject startup visibly.

## Source seams at reviewed main

| Source seam | Current behavior | Required implementation seam |
|---|---|---|
| `src/config.rs::QueueConfig::accounting_journal`; `config.toml`; `config.example.toml` | The configured/default value is a relative flat path. | Require and persist one absolute private root per replica; reject relative or changed roots. |
| `src/main.rs::serve`; `src/deferred.rs::DeferredQueues::start` | Replay/truncation runs before listeners, then one append writer opens the same path. | Resolve and qualify ownership before queue startup; publish a locked generation before appending. |
| `src/deferred.rs::{replay_accounting_journal,start_accounting_journal}` | Replay reads the shared file and truncates it; the writer flushes but does not coordinate or sync directories. | Replay only an unlocked generation under #485 evidence; retain failed artifacts; never truncate a live file. |
| `src/storage/db.rs::DatabasePool::insert_background_event` | Duplicate IDs use `ON CONFLICT(id) DO NOTHING`; differing content is not detected. | Leave duplicate/replay result semantics to #485/#87; a successful call is not completion evidence. |

## Acceptance boundary

**Recommended for acceptance:** private absolute local root per replica, persisted installation identity, per-generation locked journals, fail-closed missing evidence, ready-before-healthy-old-exit, and an explicit offline legacy migration. **Not recommended for acceptance:** shared roots, network filesystems without their own qualification, PID/timestamp ownership guesses, or cross-host recovery after the private root moves.

The future qualification must use two real processes and prove new-ready-before-old-exit, unchanged live-journal bytes, single-owner orphan discovery, and fail-closed behavior after missing or expired evidence. A proposed command is:

```sh
cargo test --all-features --test e2e_accounting_ownership same_replica_replacement_preserves_accounting_ownership -- --exact --nocapture
```

That target does not exist and is not passing evidence. Dispatch remains blocked until this proposal is accepted, #483 supplies the incident policy, #485 supplies completion/replay evidence, and each named platform/filesystem qualification passes. Acceptance authority is David/Cos through review and merge of this exact artifact.
