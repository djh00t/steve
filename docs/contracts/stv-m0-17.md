# STV-M0-17: accounting journal ownership working design baseline

**Status: owner-selected working design baseline for M1 engineering.** Final operator and release acceptance occurs once, at the combined touchable M1 release gate. This revision was checked against main `675533dbe0e2d00af9ec71ca59eb34d9151ba63f` and defines the ownership and replacement boundary for #484 only. It does not claim an implementation, a platform guarantee, a #483 incident policy, or a #485 replay/completion rule.

## Working design baseline

Give every replica one private, persistent, absolute journal root. A root belongs to one host and one local filesystem. Replicas may use the same deployment template, but their resolved roots must be different. The root contains:

- a persisted installation identity and coordination lock;
- the replica-scoped incident evidence owned by the #483 contract; and
- one journal and state record per process generation.

The `queues.accounting_journal` value must resolve to an absolute path before startup. The current relative default (`data/accounting-overflow.jsonl`) has no stable meaning across working directories and is not an acceptable adopted value. The implementation should reject a relative value visibly rather than silently choosing a working directory. The resolved path and installation identity must remain stable across process generations.

An explicit first-install provisioning step is the only trusted exception to the missing-root rule. With an absolute destination selected, no legacy source present, and no prior ownership artifacts, that step creates the root and coordination lock, writes the installation identity and initial no-current-incident evidence in #483's working-baseline representation under the lock, syncs the records and directory, and records a provisioning manifest binding the operator, root path, empty inventory, owner ID, initial revision, and sync results before the first generation starts. Ordinary startup must not infer first install from an absent or empty path. After provisioning, an absent root, a root that no longer resolves to the persisted root, an unsupported filesystem, or missing/corrupt expected ownership or incident evidence fails closed. A later process must not recreate a missing record as healthy, replay an unknown file, or truncate anything to make readiness pass. Moving a replica to another host is supported only when the same root and qualified filesystem are still available; cross-host orphan discovery and recovery are out of scope. An explicit offline migration is required when they are not.

This baseline is deliberately separate from the sibling contracts:

- #483 chooses incident state names, admission ordering, restart lifetime, readiness/status responses, and operator disposition. #484 supplies durable ownership and revision-checked evidence to that policy; it does not choose those transitions.
- #485 chooses drain, replay acknowledgement, completion evidence, retry interaction, and retirement ordering. #484 never treats a database `Ok`, channel send, flush, or released OS lock as replay completion.

## Generation ownership and replacement

For each generation, create a private staging file, acquire its exclusive OS lock, and publish it atomically under a generation UUID only after the lock is held. Write the corresponding `active` state and sync the required file and directory before readiness. Append only to the published file while holding its lock. A scanner uses a nonblocking lock attempt; a busy file belongs to its writer or replayer and is not read, truncated, or retired.

The handoff is:

1. The new process resolves and verifies the private root, installation identity, coordination lock, incident evidence, and its own generation staging file.
2. It publishes and syncs its active generation, then takes a complete evidence snapshot and may become ready while the healthy predecessor remains alive.
3. The predecessor drains under #485 and releases its generation lock only after #485's required completion evidence is durable.
4. A successor may inspect an unlocked predecessor file only after checking its generation state and the current incident evidence under the coordination lock. Missing, corrupt, non-clean, or changed evidence is reported to #483 as unresolved; an empty journal does not prove that volatile work was absent.
5. Replay and retirement occur only under #485's required acknowledgement and retirement ordering. A live journal is never truncated.

The coordination record uses a monotonic revision. Reads and read/modify/write updates take the coordination lock and reread the current revision; a stale clear, acknowledgement, or evidence update cannot overwrite a newer failure. Every disposition or closure record carries the incident revision and exact coverage tuples `(generation_id, generation_state_revision, journal_evidence_digest)`. A generation state revision advances when its journal, state, or completion evidence changes. An unchanged generation whose tuple is covered remains covered; a new generation, changed tuple, or uncovered artifact creates new unresolved evidence. The lock protects serialization only. It does not itself prove a clean exit, a durable append, or an incident transition.

The proposed timing values below are policy inputs for review, not guarantees from this document:

- poll complete incident and generation evidence every **50 ms**;
- consider the in-memory snapshot fresh for **100 ms** from poll start; and
- use **100 ms** as the proposed coordination-lock acquisition timeout.

An incomplete or expired snapshot and every lock, read, write, sync, or atomic-publication error fail closed. No observation bound is claimed when a process is unscheduled, when publication has not succeeded, or before the named platform qualification exists.

An abandoned pre-publication `.staging` file may be cleaned only under the coordination lock after a nonblocking exclusive lock succeeds, its length is zero, and neither its canonical journal nor its generation-state record exists. Sync the containing directory after removal. A busy or nonempty staging file, or any file with canonical/state evidence, conflicting metadata, or an unknown format is retained and fails closed. This narrow cleanup applies only to an interrupted empty pre-publication file; it never applies to incident-record temporary files or published journals.

## First adoption of the legacy flat file

The current flat file is written and replayed without ownership coordination. It cannot overlap with the new protocol. Its first adoption is a one-time offline maintenance operation, not an automatic startup migration. The legacy source remains a regular file at its existing absolute path; the new journal root is a distinct destination, recommended as the sibling path `<legacy-source>.root`. The migration manifest records both paths, and the post-adoption configuration points to the destination root. The legacy file is never treated as a directory or given ownership children:

1. Stop every pre-protocol writer and record the maintenance boundary. Do not proceed without evidence that no old binary can append.
2. Preserve the original bytes as an immutable backup beside the source. Record its byte length and hash, sync the backup, and sync its containing directory. Leave the source untouched.
3. Provision the distinct destination root and coordination lock under the first-install procedure. Under that lock, create and lock a staging generation. Parse the backup; any framing or torn-tail decision remains with #85. Write the migrated records to the staging generation, sync it and its directory, and retain the source and backup.
4. Reconcile records under #485's replay and acknowledgement contract. Write an adoption marker binding the source hash and length, target generation ID, record outcome, and evidence reference; sync the marker and its directory.
5. Only after that marker is durable, verify that the source still has the recorded hash, atomically move it to a retained legacy name outside active discovery, and sync the source directory. Publish the adopted generation and permit normal rolling overlap.

A crash before the adoption marker leaves the source and backup authoritative; startup fails closed and the operation resumes from the backup. A crash after the marker but before the source move or final directory sync also fails closed until the marker, source hash, target root, target generation, and directory evidence are reconciled. There is no auto-truncate, delete, or silent `clear`. Accepted migration evidence is the stopped-writer record, source and destination paths, source hash/length, durable backup, parser outcome, #485 replay acknowledgement, target generation ID, adoption marker, and final directory sync. If any item is absent, the migration is incomplete.

## Platform and durability boundary

No OS/filesystem pair is supported by this baseline. Qualification must pass separately for every deployment pair, with the baseline named explicitly as Linux/ext4, macOS/APFS, and Windows/NTFS. Each pair must demonstrate exclusive lock behavior, lock retention across staging publication, concurrent append visibility, file and directory sync ordering, atomic replacement, and lock release after process death. A deployment using another filesystem needs its own result before it is supported.

CI can exercise the protocol and failure ordering; CI alone cannot prove any power-loss guarantee. Power loss during append, database commit, publication, or retirement remains unknown until separately qualified with the actual OS, filesystem, and storage path. Unsupported lock or sync semantics must reject startup visibly.

## Source seams at reviewed main

| Source seam | Current behavior | Required implementation seam |
|---|---|---|
| `src/config.rs::QueueConfig::accounting_journal`; `config.toml`; `config.example.toml` | The configured/default value is a relative flat path. | Keep the legacy source file and new destination root distinct; require and persist the absolute root after adoption, rejecting relative or changed roots. |
| `src/main.rs::serve`; `src/deferred.rs::DeferredQueues::start` | Replay/truncation runs before listeners, then one append writer opens the same path. | Resolve and qualify ownership before queue startup; publish a locked generation before appending. |
| `src/deferred.rs::{replay_accounting_journal,start_accounting_journal}` | Replay reads the shared file and truncates it; the writer flushes but does not coordinate or sync directories. | Replay only an unlocked generation under #485 evidence; retain failed artifacts; never truncate a live file. |
| `src/storage/db.rs::DatabasePool::insert_background_event` | Duplicate IDs use `ON CONFLICT(id) DO NOTHING`; differing content is not detected. | Leave duplicate/replay result semantics to #485/#87; a successful call is not completion evidence. |

## Working-baseline boundary

**Included:** private absolute local root per replica, persisted installation identity, per-generation locked journals, fail-closed missing evidence, ready-before-healthy-old-exit, and an explicit offline legacy migration. **Excluded:** shared roots, network filesystems without their own qualification, PID/timestamp ownership guesses, and cross-host recovery after the private root moves.

The future qualification must use two real processes and prove new-ready-before-old-exit, unchanged live-journal bytes, single-owner orphan discovery, and fail-closed behavior after missing or expired evidence. A proposed command is:

```sh
cargo test --all-features --test e2e_accounting same_replica_replacement_preserves_accounting_ownership -- --exact --nocapture
```

That target does not exist and is not passing evidence. Runtime completion remains blocked until #483 supplies the incident policy, #485 supplies completion/replay evidence, and each named platform/filesystem qualification passes. Final operator and release acceptance occurs at the combined M1 release gate.
