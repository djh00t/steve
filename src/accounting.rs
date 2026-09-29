use crate::storage::{DatabasePool, InsertBackgroundEvent};
use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const FORMAT_VERSION: u64 = 1;
const LOCK_TIMEOUT: Duration = Duration::from_millis(100);
const LOCK_POLL: Duration = Duration::from_millis(50);
const PRODUCTION_SNAPSHOT_FRESHNESS: Duration = Duration::from_millis(100);
#[cfg(not(test))]
const SNAPSHOT_FRESHNESS: Duration = PRODUCTION_SNAPSHOT_FRESHNESS;
#[cfg(test)]
const SNAPSHOT_FRESHNESS: Duration = Duration::from_secs(1); // Parallel unit tests can be unscheduled.

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AccountingEvent {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) payload: Value,
    pub(crate) created_at: String,
}

impl AccountingEvent {
    pub(crate) fn new(kind: &'static str, payload: Value) -> Self {
        Self {
            id: Uuid::now_v7().to_string(),
            kind: kind.to_string(),
            payload,
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug)]
struct ParsedFrame {
    offset: u64,
    length: u64,
    digest: String,
    event: AccountingEvent,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct FrameEvidence {
    offset: u64,
    length: u64,
    digest: String,
}

#[derive(Debug)]
struct ParsedJournal {
    frames: Vec<ParsedFrame>,
    malformed: Vec<FrameEvidence>,
    torn_tail: Option<FrameEvidence>,
    complete_boundary: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReplayOutcome {
    Inserted,
    DuplicateIdentical,
    DuplicateConflict,
    Failed,
    Unknown,
}

impl ReplayOutcome {
    fn acknowledges(&self) -> bool {
        matches!(self, Self::Inserted | Self::DuplicateIdentical)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReplayReceipt {
    generation_id: String,
    offset: u64,
    length: u64,
    record_digest: String,
    event_id: String,
    content_digest: String,
    outcome: ReplayOutcome,
    attempts: u64,
    error: Option<String>,
    database_evidence_ref: Option<String>,
    database_evidence_at: Option<String>,
    conflict_snapshot_ref: Option<String>,
    conflict_snapshot_digest: Option<String>,
    #[serde(default)]
    resolution_ref: Option<String>,
    #[serde(default)]
    resolution_digest: Option<String>,
    #[serde(default)]
    resolved_authoritative: Option<String>,
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReplayManifest {
    format_version: u64,
    installation_id: String,
    generation_id: String,
    revision: u64,
    source_coordination_revision: u64,
    source_generation_revision: u64,
    journal_length: u64,
    journal_evidence_digest: String,
    complete_boundary: u64,
    complete_record_count: u64,
    malformed_frames: Vec<FrameEvidence>,
    torn_tail_offset: Option<u64>,
    torn_tail_length: u64,
    torn_tail_digest: Option<String>,
    database_durability: Value,
    receipts: Vec<ReplayReceipt>,
    ordered_receipt_digest: String,
    outcome_counts: BTreeMap<String, u64>,
    parser_result: String,
    sync_result: String,
    retry_exhausted: bool,
    updated_at: String,
}

#[derive(Clone, Copy)]
struct ReplayPolicy {
    operation_timeout: Duration,
    retry_deadline: Duration,
    retry_interval: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct ConflictRowEvidence {
    id: String,
    kind: String,
    payload: String,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecordedConflictSnapshot {
    format_version: u64,
    installation_id: String,
    generation_id: String,
    journal_offset: u64,
    journal_length: u64,
    journal_record_digest: String,
    source: ConflictRowEvidence,
    existing: ConflictRowEvidence,
    existing_row_digest: String,
    verification_id: String,
    backend: String,
    transaction_isolation: String,
    read_evidence: String,
    observed_at: String,
}

fn parse_journal(bytes: &[u8]) -> ParsedJournal {
    let mut frames = Vec::new();
    let mut malformed = Vec::new();
    let mut offset = 0;

    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.last() != Some(&b'\n') {
            break;
        }
        let evidence = FrameEvidence {
            offset: offset as u64,
            length: line.len() as u64,
            digest: digest(line),
        };
        match serde_json::from_slice(&line[..line.len() - 1]) {
            Ok(event) => frames.push(ParsedFrame {
                offset: evidence.offset,
                length: evidence.length,
                digest: evidence.digest,
                event,
            }),
            Err(_) => malformed.push(evidence),
        }
        offset += line.len();
    }

    ParsedJournal {
        frames,
        malformed,
        torn_tail: (offset < bytes.len()).then(|| FrameEvidence {
            offset: offset as u64,
            length: (bytes.len() - offset) as u64,
            digest: digest(&bytes[offset..]),
        }),
        complete_boundary: offset as u64,
    }
}

#[derive(Clone)]
pub(crate) struct AccountingCoordinator {
    root: PathBuf,
    generation_id: String,
    journal: SyncSender<JournalMessage>,
    journal_writer: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    accepting: Arc<AtomicBool>,
    state: Arc<Mutex<GenerationState>>,
    incident: Arc<Mutex<Incident>>,
    incident_publisher: mpsc::Sender<IncidentPublisherMessage>,
    incident_publisher_thread: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    incident_submitted: Arc<AtomicU64>,
    _incident_durable: Arc<(Mutex<u64>, Condvar)>,
    _serving: AccountingServingGuard,
}

#[derive(Clone)]
pub(crate) struct AccountingServingGuard {
    _file: Arc<File>,
}

pub(crate) struct AccountingOfflineGuard {
    _file: File,
}

#[derive(Clone)]
struct IncidentPublication {
    sequence: u64,
    publication_id: String,
    based_on_revision: u64,
    attempted: Incident,
    cause: IncidentCause,
    lost: u64,
    unknown: u64,
}

enum JournalMessage {
    Event(AccountingEvent),
    Shutdown(SyncSender<()>),
}

enum IncidentPublisherMessage {
    Publish(Box<IncidentPublication>),
    Shutdown,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) struct AccountingSnapshot {
    pub(crate) installation_id: String,
    pub(crate) revision: u64,
    pub(crate) coverage: Vec<Coverage>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Coverage {
    pub(crate) generation_id: String,
    pub(crate) generation_state_revision: u64,
    pub(crate) journal_evidence_digest: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Installation {
    format_version: u64,
    installation_id: String,
    root: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Incident {
    format_version: u64,
    installation_id: String,
    revision: u64,
    state: IncidentState,
    incident_id: Option<String>,
    first_observed_at: Option<String>,
    cause: Option<IncidentCause>,
    disposition: Option<Disposition>,
    payloads: IncidentPayloads,
    #[serde(default)]
    publication_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum IncidentState {
    Clear,
    Blocked,
    Unreconciled,
    Acknowledged,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IncidentCause {
    PrimaryAndJournalUnavailable,
    PrimaryPersistenceFailedAndJournalUnavailable,
    JournalWriteFailed,
    PriorIncidentUnreconciled,
    ReplayContentConflict,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IncidentPayloads {
    pending_replay: PendingReplay,
    provisional: Provisional,
    outcome_totals: OutcomeTotals,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PendingReplay {
    volatile: Option<u64>,
    durable: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Provisional {
    unknown: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OutcomeTotals {
    reconciled: Option<u64>,
    unrecoverable_lost: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Disposition {
    kind: String,
    actor: String,
    at: String,
    evidence_ref: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AuditRecord {
    format_version: u64,
    installation_id: String,
    operation: String,
    incident_id: String,
    revision: u64,
    previous_revision: u64,
    coverage: Vec<Coverage>,
    disposition: Option<Disposition>,
    event_id: Option<String>,
    authoritative: Option<String>,
    evidence_ref: String,
    conflict_snapshot_ref: Option<String>,
    conflict_snapshot_digest: Option<String>,
    final_row: Option<ConflictRowEvidence>,
    actor: String,
    at: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Provisioning {
    format_version: u64,
    installation_id: String,
    operator: String,
    root: String,
    inventory: Vec<String>,
    revision: u64,
    created_at: String,
    files_synced: bool,
    directory_synced: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Coordination {
    format_version: u64,
    installation_id: String,
    revision: u64,
    coverage: Vec<Coverage>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct GenerationState {
    format_version: u64,
    installation_id: String,
    generation_id: String,
    revision: u64,
    phase: GenerationPhase,
    journal: String,
    journal_length: u64,
    journal_evidence_digest: String,
    #[serde(default)]
    admitted_count: Option<u64>,
    #[serde(default)]
    worker_completed_count: Option<u64>,
    #[serde(default)]
    journal_synced_count: Option<u64>,
    #[serde(default)]
    database_committed_count: Option<u64>,
    #[serde(default)]
    last_complete_record_boundary: Option<u64>,
    #[serde(default)]
    replay_manifest: Option<String>,
    #[serde(default)]
    replay_manifest_digest: Option<String>,
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GenerationPhase {
    Active,
    Draining,
    Adopting,
    Reconciled,
}

#[derive(Debug, Deserialize, Serialize)]
struct Adoption {
    format_version: u64,
    installation_id: String,
    state: String,
    source: String,
    backup: String,
    source_length: u64,
    source_digest: String,
    maintenance_assertion: MaintenanceAssertion,
    generation_id: String,
    record_outcome: String,
    accepted_evidence_ref: Option<String>,
    retained_source: Option<String>,
    source_directory_synced: bool,
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct MaintenanceAssertion {
    assertion_kind: String,
    path: String,
    digest: String,
    workload_identity: String,
    host: String,
    source: String,
    stopped: bool,
    restart_disabled: bool,
    observed_at: String,
    command_or_exported_status: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintenanceAssertionInput {
    workload_identity: String,
    host: String,
    source: String,
    stopped: bool,
    restart_disabled: bool,
    observed_at: String,
    command_or_exported_status: String,
}

struct AdoptionSeed {
    state: String,
    source: String,
    backup: String,
    source_length: u64,
    source_digest: String,
    maintenance_assertion: MaintenanceAssertion,
    generation_id: String,
    record_outcome: String,
}

fn clear_incident(installation_id: &str) -> Incident {
    Incident {
        format_version: FORMAT_VERSION,
        installation_id: installation_id.to_string(),
        revision: 1,
        state: IncidentState::Clear,
        incident_id: None,
        first_observed_at: None,
        cause: None,
        disposition: None,
        payloads: IncidentPayloads {
            pending_replay: PendingReplay {
                volatile: Some(0),
                durable: Some(0),
            },
            provisional: Provisional { unknown: Some(0) },
            outcome_totals: OutcomeTotals {
                reconciled: Some(0),
                unrecoverable_lost: Some(0),
            },
        },
        publication_ids: BTreeSet::new(),
    }
}

fn public_incident(incident: &Incident) -> Value {
    let mut value = serde_json::to_value(incident).expect("incident must serialize");
    if let Some(disposition) = value.get_mut("disposition").and_then(Value::as_object_mut) {
        disposition.insert("actor".into(), Value::Null);
        disposition.insert("evidence_ref".into(), Value::Null);
    }
    let object = value.as_object_mut().expect("incident must be an object");
    object.remove("format_version");
    object.remove("installation_id");
    object.remove("revision");
    object.remove("publication_ids");
    value
}

fn refresh_incident(root: &Path, incident: &mut Incident) -> bool {
    match read_checked::<Incident>(&root.join("incident.json")) {
        Ok(persisted) => {
            let newer_unresolved_lineage = matches!(
                incident.state,
                IncidentState::Clear | IncidentState::Acknowledged
            ) && matches!(
                persisted.state,
                IncidentState::Blocked | IncidentState::Unreconciled
            ) && incident.incident_id != persisted.incident_id;
            if persisted.revision > incident.revision
                && (newer_unresolved_lineage
                    || incident
                        .publication_ids
                        .is_subset(&persisted.publication_ids))
            {
                *incident = persisted;
            }
            matches!(has_unowned_incomplete_generation(root, incident), Ok(false))
        }
        Err(_) => false,
    }
}

fn has_unowned_incomplete_generation(root: &Path, incident: &Incident) -> Result<bool> {
    let acknowledged_coverage = if incident.state == IncidentState::Acknowledged {
        let records: Vec<AuditRecord> = read_checked(&root.join("audit.json"))?;
        Some(
            records
                .into_iter()
                .find(|record| {
                    record.operation == "acknowledge"
                        && Some(record.incident_id.as_str()) == incident.incident_id.as_deref()
                        && record.revision == incident.revision
                        && record.disposition == incident.disposition
                })
                .context("acknowledged incident audit is unavailable")?
                .coverage,
        )
    } else {
        None
    };
    let mut state_paths = BTreeMap::new();
    let mut journals = BTreeSet::new();
    for entry in fs::read_dir(root.join("generations"))? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(true);
        };
        if let Some(generation_id) = name.strip_suffix(".state.json") {
            state_paths.insert(generation_id.to_string(), path);
        } else if let Some(generation_id) = name.strip_suffix(".journal") {
            journals.insert(generation_id.to_string());
        }
    }
    let state_ids = state_paths.keys().cloned().collect::<BTreeSet<_>>();
    let coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    let coverage_ids = coordination
        .coverage
        .iter()
        .map(|entry| entry.generation_id.clone())
        .collect::<BTreeSet<_>>();
    if journals != state_ids || coverage_ids != state_ids {
        return Ok(true);
    }
    for path in state_paths.values() {
        let state: GenerationState = read_checked(path)?;
        if !matches!(
            state.phase,
            GenerationPhase::Active | GenerationPhase::Draining | GenerationPhase::Adopting
        ) {
            continue;
        }
        if acknowledged_coverage
            .as_ref()
            .is_some_and(|coverage_entries| coverage_entries.contains(&coverage(&state)))
        {
            continue;
        }
        let journal = OpenOptions::new()
            .read(true)
            .write(true)
            .open(generation_journal_path(root, &state.generation_id))?;
        match journal.try_lock() {
            Ok(()) => {
                File::unlock(&journal)?;
                return Ok(true);
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).context("checking live accounting generation ownership")
            }
        }
    }
    Ok(false)
}

fn fail_closed_incident(incident: &Incident) -> Value {
    let mut unavailable = incident.clone();
    unavailable.state = IncidentState::Unreconciled;
    unavailable.cause = Some(IncidentCause::PriorIncidentUnreconciled);
    unavailable.disposition = None;
    unavailable.payloads = IncidentPayloads {
        pending_replay: PendingReplay {
            volatile: None,
            durable: None,
        },
        provisional: Provisional { unknown: None },
        outcome_totals: OutcomeTotals {
            reconciled: None,
            unrecoverable_lost: None,
        },
    };
    public_incident(&unavailable)
}

fn queue_incident_publication(
    shared: &Mutex<Incident>,
    publisher: &mpsc::Sender<IncidentPublisherMessage>,
    submitted: &AtomicU64,
    cause: IncidentCause,
    lost: u64,
    unknown: u64,
) -> Result<()> {
    let mut shared = shared.lock().expect("accounting incident");
    let based_on_revision = shared.revision;
    let publication_id = Uuid::now_v7().to_string();
    apply_incident_failure(&mut shared, cause, lost, unknown);
    shared.publication_ids.insert(publication_id.clone());
    let sequence = submitted.load(Ordering::Acquire).saturating_add(1);
    publisher
        .send(IncidentPublisherMessage::Publish(Box::new(
            IncidentPublication {
                sequence,
                publication_id,
                based_on_revision,
                attempted: shared.clone(),
                cause,
                lost,
                unknown,
            },
        )))
        .context("queueing durable accounting incident publication")?;
    submitted.store(sequence, Ordering::Release);
    Ok(())
}

fn persist_incident_publication(
    root: &Path,
    publication: &IncidentPublication,
) -> Result<Incident> {
    let lock = open_coordination_lock(root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    let persisted: Incident = read_checked(&root.join("incident.json"))?;
    if persisted
        .publication_ids
        .contains(&publication.publication_id)
    {
        File::unlock(&lock).context("unlocking accounting coordination")?;
        return Ok(persisted);
    }
    if persisted.revision < publication.based_on_revision {
        bail!("durable accounting incident revision moved backwards");
    }
    let candidate = if persisted.revision == publication.based_on_revision {
        publication.attempted.clone()
    } else {
        let mut candidate = persisted;
        apply_incident_failure(
            &mut candidate,
            publication.cause,
            publication.lost,
            publication.unknown,
        );
        candidate
            .publication_ids
            .insert(publication.publication_id.clone());
        candidate
    };
    write_checked(&root.join("incident.json"), &candidate)?;
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(candidate)
}

fn start_incident_publisher(
    root: PathBuf,
    receiver: mpsc::Receiver<IncidentPublisherMessage>,
    shared: Arc<Mutex<Incident>>,
    durable: Arc<(Mutex<u64>, Condvar)>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("steve-accounting-incident".into())
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                let IncidentPublisherMessage::Publish(publication) = message else {
                    break;
                };
                let persisted = loop {
                    match persist_incident_publication(&root, &publication) {
                        Ok(persisted) => break persisted,
                        Err(error) => {
                            tracing::warn!(%error, "retrying durable accounting incident publication");
                            thread::sleep(LOCK_POLL);
                        }
                    }
                };
                let mut current = shared.lock().expect("accounting incident");
                if persisted.revision > current.revision
                    && current
                        .publication_ids
                        .is_subset(&persisted.publication_ids)
                {
                    *current = persisted;
                }
                drop(current);
                let (completed, changed) = &*durable;
                let mut completed = completed.lock().expect("incident publication progress");
                *completed = (*completed).max(publication.sequence);
                changed.notify_all();
            }
        })
        .context("spawning accounting incident publisher")
}

fn apply_incident_failure(incident: &mut Incident, cause: IncidentCause, lost: u64, unknown: u64) {
    if matches!(
        incident.state,
        IncidentState::Clear | IncidentState::Acknowledged
    ) {
        incident.publication_ids.clear();
        incident.incident_id = Some(Uuid::now_v7().to_string());
        incident.first_observed_at = Some(Utc::now().to_rfc3339());
        incident.cause = Some(cause);
        incident.disposition = None;
        incident.payloads.pending_replay = PendingReplay {
            volatile: Some(0),
            durable: Some(0),
        };
        incident.payloads.provisional = Provisional { unknown: Some(0) };
        incident.payloads.outcome_totals = OutcomeTotals {
            reconciled: Some(0),
            unrecoverable_lost: Some(0),
        };
    }
    incident.state = IncidentState::Blocked;
    incident.revision = incident.revision.saturating_add(1);
    incident.payloads.provisional.unknown = incident
        .payloads
        .provisional
        .unknown
        .map(|value| value.saturating_add(unknown));
    incident.payloads.outcome_totals.unrecoverable_lost = incident
        .payloads
        .outcome_totals
        .unrecoverable_lost
        .map(|value| value.saturating_add(lost));
}

fn latch_replay_conflict(root: &Path, installation: &Installation) -> Result<()> {
    let lock = open_coordination_lock(root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    let mut incident: Incident = read_checked(&root.join("incident.json"))?;
    if incident.installation_id != installation.installation_id {
        bail!("replay conflict incident identity changed");
    }
    let opening = matches!(
        incident.state,
        IncidentState::Clear | IncidentState::Acknowledged
    );
    if opening {
        incident.publication_ids.clear();
        incident.incident_id = Some(Uuid::now_v7().to_string());
        incident.first_observed_at = Some(Utc::now().to_rfc3339());
        incident.cause = Some(IncidentCause::ReplayContentConflict);
        incident.disposition = None;
        incident.payloads = IncidentPayloads {
            pending_replay: PendingReplay {
                volatile: Some(0),
                durable: Some(0),
            },
            provisional: Provisional { unknown: Some(0) },
            outcome_totals: OutcomeTotals {
                reconciled: Some(0),
                unrecoverable_lost: Some(0),
            },
        };
    }
    let unresolved = unresolved_conflict_count(root)?;
    if opening || incident.payloads.pending_replay.durable != Some(unresolved) {
        incident.revision = incident.revision.saturating_add(1);
        if opening {
            incident.state = IncidentState::Blocked;
        }
        incident.payloads.pending_replay.durable = Some(unresolved);
        write_checked(&root.join("incident.json"), &incident)?;
    }
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(())
}

fn unresolved_conflict_count(root: &Path) -> Result<u64> {
    let mut count = 0;
    for entry in fs::read_dir(root.join("generations"))? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".replay.json"))
        {
            continue;
        }
        let manifest: ReplayManifest = read_checked(&path)?;
        count += manifest
            .receipts
            .iter()
            .filter(|receipt| {
                receipt.outcome == ReplayOutcome::DuplicateConflict
                    && receipt.resolution_ref.is_none()
            })
            .count() as u64;
    }
    Ok(count)
}

fn publish_unreconciled(root: &Path) -> Result<()> {
    let lock = open_coordination_lock(root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    let mut incident: Incident = read_checked(&root.join("incident.json"))?;
    incident.revision = incident.revision.saturating_add(1);
    incident.state = IncidentState::Unreconciled;
    incident.incident_id = None;
    incident.first_observed_at = None;
    incident.cause = Some(IncidentCause::PriorIncidentUnreconciled);
    incident.disposition = None;
    incident.payloads = IncidentPayloads {
        pending_replay: PendingReplay {
            volatile: None,
            durable: None,
        },
        provisional: Provisional { unknown: None },
        outcome_totals: OutcomeTotals {
            reconciled: None,
            unrecoverable_lost: None,
        },
    };
    write_checked(&root.join("incident.json"), &incident)?;
    File::unlock(&lock).context("unlocking accounting coordination")
}

fn publish_abandoned_active(root: &Path, state: &GenerationState) -> Result<()> {
    let lock = open_coordination_lock(root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    let mut incident: Incident = read_checked(&root.join("incident.json"))?;
    let publication_id = format!(
        "abandoned:{}:{}:{}",
        state.generation_id, state.revision, state.journal_evidence_digest
    );
    if incident.publication_ids.contains(&publication_id) {
        File::unlock(&lock).context("unlocking accounting coordination")?;
        return Ok(());
    }
    if incident.state != IncidentState::Acknowledged {
        if incident.state == IncidentState::Clear {
            incident.publication_ids.clear();
            incident.incident_id = Some(Uuid::now_v7().to_string());
            incident.first_observed_at = Some(Utc::now().to_rfc3339());
            incident.payloads.outcome_totals.unrecoverable_lost = Some(0);
        }
        incident.revision = incident.revision.saturating_add(1);
        incident.state = IncidentState::Unreconciled;
        incident.cause = Some(IncidentCause::PriorIncidentUnreconciled);
        incident.disposition = None;
        incident.payloads.pending_replay = PendingReplay {
            volatile: None,
            durable: None,
        };
        incident.payloads.provisional.unknown = None;
        incident.payloads.outcome_totals.reconciled = None;
        incident.publication_ids.insert(publication_id);
        write_checked(&root.join("incident.json"), &incident)?;
    }
    File::unlock(&lock).context("unlocking accounting coordination")
}

impl AccountingCoordinator {
    pub(crate) fn provision(root: &Path) -> Result<()> {
        provision_root(root, None)
    }

    pub(crate) fn adopt_legacy(
        source: &Path,
        root: &Path,
        maintenance_assertion: &Path,
    ) -> Result<String> {
        adopt_legacy(source, root, maintenance_assertion)
    }

    pub(crate) fn acknowledge(
        root: &Path,
        incident_id: &str,
        revision: u64,
        kind: &str,
        evidence_ref: &str,
    ) -> Result<Value> {
        acknowledge_incident(root, incident_id, revision, kind, evidence_ref)
    }

    pub(crate) fn ensure_offline(root: &Path) -> Result<AccountingOfflineGuard> {
        ensure_offline(root)
    }

    pub(crate) fn acquire_serving(root: &Path) -> Result<AccountingServingGuard> {
        let root = resolve_accounting_root(root)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("maintenance.lock"))?;
        file.lock_shared()
            .context("locking accounting serving lease")?;
        Ok(AccountingServingGuard {
            _file: Arc::new(file),
        })
    }

    pub(crate) fn audit(root: &Path, incident_id: &str, revision: u64) -> Result<Value> {
        audit_incident(root, incident_id, revision)
    }

    pub(crate) async fn resolve_conflict(
        root: &Path,
        database: &DatabasePool,
        incident_id: &str,
        revision: u64,
        event_id: &str,
        authoritative: &str,
        evidence_ref: &str,
    ) -> Result<Value> {
        resolve_conflict(
            root,
            database,
            incident_id,
            revision,
            event_id,
            authoritative,
            evidence_ref,
        )
        .await
    }

    pub(crate) async fn reconcile_startup(
        root: &Path,
        database: &DatabasePool,
        operation_timeout: Duration,
        retry_deadline: Duration,
        retry_interval: Duration,
        restore_prior_incident: bool,
    ) -> Result<()> {
        reconcile_startup(
            root,
            database,
            operation_timeout,
            retry_deadline,
            retry_interval,
            restore_prior_incident,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn start(root: &Path, capacity: usize) -> Result<Self> {
        let serving = Self::acquire_serving(root)?;
        Self::start_with_guard(root, capacity, serving)
    }

    pub(crate) fn start_with_guard(
        root: &Path,
        capacity: usize,
        serving: AccountingServingGuard,
    ) -> Result<Self> {
        let root = resolve_accounting_root(root)?;
        let coordination_lock = open_coordination_lock(&root)?;
        lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
        recover_checked_publications(&root)?;
        let installation = load_provisioned_root_locked(&root)?;
        let incident: Incident = read_checked(&root.join("incident.json"))?;
        if root.join("adoption.json").exists() {
            let adoption: Adoption = read_checked(&root.join("adoption.json"))?;
            if adoption.state != "complete" && incident.state == IncidentState::Clear {
                bail!(
                    "legacy accounting adoption is {}; pending reconciliation",
                    adoption.state
                );
            }
            if adoption.state == "complete" {
                validate_complete_adoption(&root, &installation, &adoption)?;
            }
        }

        cleanup_empty_staging(&root)?;
        let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
        let states = verify_generations_with_adoption(
            &root,
            &installation,
            &coordination,
            incident.state != IncidentState::Clear,
        )?;
        let generation_id = Uuid::now_v7().to_string();
        let journal_path = generation_journal_path(&root, &generation_id);
        let staging_path = journal_path.with_extension("journal.staging");
        let journal_file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(&staging_path)
            .with_context(|| format!("creating journal staging {}", staging_path.display()))?;
        journal_file.lock().context("locking generation journal")?;
        fs::rename(&staging_path, &journal_path).with_context(|| {
            format!(
                "publishing journal {} as {}",
                staging_path.display(),
                journal_path.display()
            )
        })?;
        sync_directory(&root.join("generations"))?;

        let read_only = matches!(
            incident.state,
            IncidentState::Blocked | IncidentState::Unreconciled
        );
        let state = GenerationState {
            format_version: FORMAT_VERSION,
            installation_id: installation.installation_id.clone(),
            generation_id: generation_id.clone(),
            revision: 1,
            phase: if read_only {
                GenerationPhase::Reconciled
            } else {
                GenerationPhase::Active
            },
            journal: journal_path.display().to_string(),
            journal_length: 0,
            journal_evidence_digest: digest(&[]),
            admitted_count: read_only.then_some(0),
            worker_completed_count: read_only.then_some(0),
            journal_synced_count: read_only.then_some(0),
            database_committed_count: read_only.then_some(0),
            last_complete_record_boundary: read_only.then_some(0),
            replay_manifest: None,
            replay_manifest_digest: None,
            updated_at: Utc::now().to_rfc3339(),
        };
        write_checked(&generation_state_path(&root, &generation_id), &state)?;
        coordination.revision += 1;
        coordination.coverage = states.values().map(coverage).collect();
        coordination.coverage.push(coverage(&state));
        sort_coverage(&mut coordination.coverage);
        write_checked(&root.join("coordination.json"), &coordination)?;
        let started = Instant::now();
        let published_coordination: Coordination = read_checked(&root.join("coordination.json"))?;
        let published_state: GenerationState =
            read_checked(&generation_state_path(&root, &generation_id))?;
        if published_coordination.revision != coordination.revision
            || published_state.revision != state.revision
            || !published_coordination
                .coverage
                .contains(&coverage(&published_state))
        {
            bail!("published accounting ownership snapshot is inconsistent");
        }
        ensure_fresh(started)?;
        File::unlock(&coordination_lock).context("unlocking accounting coordination")?;

        let state = Arc::new(Mutex::new(state));
        let incident = Arc::new(Mutex::new(incident));
        let incident_submitted = Arc::new(AtomicU64::new(0));
        let incident_durable = Arc::new((Mutex::new(0), Condvar::new()));
        let (incident_publisher, incident_publications) = mpsc::channel();
        let incident_publisher_thread = start_incident_publisher(
            root.clone(),
            incident_publications,
            incident.clone(),
            incident_durable.clone(),
        )?;
        let (journal, receiver) = mpsc::sync_channel::<JournalMessage>(capacity.max(1));
        let journal_writer = if read_only {
            drop(receiver);
            drop(journal_file);
            None
        } else {
            Some(start_writer(
                root.clone(),
                journal_file,
                receiver,
                state.clone(),
                incident.clone(),
                incident_publisher.clone(),
                incident_submitted.clone(),
            )?)
        };
        Ok(Self {
            root,
            generation_id,
            journal,
            journal_writer: Arc::new(Mutex::new(journal_writer)),
            accepting: Arc::new(AtomicBool::new(!read_only)),
            state,
            incident,
            incident_publisher,
            incident_publisher_thread: Arc::new(Mutex::new(Some(incident_publisher_thread))),
            incident_submitted,
            _incident_durable: incident_durable,
            _serving: serving,
        })
    }

    pub(crate) fn offer(&self, event: AccountingEvent) -> Result<(), AccountingEvent> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(event);
        }
        match self.journal.try_send(JournalMessage::Event(event)) {
            Ok(()) => Ok(()),
            Err(
                TrySendError::Full(JournalMessage::Event(event))
                | TrySendError::Disconnected(JournalMessage::Event(event)),
            ) => Err(event),
            Err(
                TrySendError::Full(JournalMessage::Shutdown(_))
                | TrySendError::Disconnected(JournalMessage::Shutdown(_)),
            ) => {
                unreachable!("offer sends only journal events")
            }
        }
    }

    pub(crate) fn latch_incident(
        &self,
        cause: IncidentCause,
        lost: u64,
        unknown: u64,
    ) -> Result<()> {
        queue_incident_publication(
            &self.incident,
            &self.incident_publisher,
            &self.incident_submitted,
            cause,
            lost,
            unknown,
        )
    }

    pub(crate) fn public_incident(&self) -> Value {
        let mut incident = self.incident.lock().expect("accounting incident");
        if refresh_incident(&self.root, &mut incident) {
            public_incident(&incident)
        } else {
            fail_closed_incident(&incident)
        }
    }

    pub(crate) fn with_incident_admission<T>(&self, admit: impl FnOnce() -> T) -> Result<T, Value> {
        let mut incident = self.incident.lock().expect("accounting incident");
        if !refresh_incident(&self.root, &mut incident) {
            return Err(fail_closed_incident(&incident));
        }
        if matches!(
            incident.state,
            IncidentState::Blocked | IncidentState::Unreconciled
        ) {
            return Err(public_incident(&incident));
        }
        Ok(admit())
    }

    #[allow(dead_code)]
    pub(crate) fn snapshot(&self) -> Result<AccountingSnapshot> {
        load_snapshot(&self.root)
    }

    pub(crate) fn begin_shutdown(&self, timeout: Duration) -> Result<AccountingSnapshot> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .context("accounting shutdown deadline is out of range")?;
        self.accepting.store(false, Ordering::Release);
        self.stop_journal_writer(deadline)?;
        self.stop_incident_publisher(deadline)?;
        let lock = open_coordination_lock(&self.root)?;
        lock_with_timeout(
            &lock,
            shutdown_remaining(deadline, "locking final accounting ownership")?.min(LOCK_TIMEOUT),
        )?;
        recover_checked_publications(&self.root)?;
        shutdown_remaining(deadline, "recovering final accounting publications")?;
        let mut coordination: Coordination = read_checked(&self.root.join("coordination.json"))?;
        let mut state = self.state.lock().expect("accounting generation state");
        if state.generation_id == self.generation_id && state.phase == GenerationPhase::Reconciled {
            File::unlock(&lock).context("unlocking accounting coordination")?;
            return Ok(AccountingSnapshot {
                installation_id: coordination.installation_id,
                revision: coordination.revision,
                coverage: coordination.coverage,
            });
        }
        if state.generation_id != self.generation_id || state.phase != GenerationPhase::Active {
            bail!("accounting generation is not active");
        }
        let persisted: GenerationState =
            read_checked(&generation_state_path(&self.root, &self.generation_id))?;
        if persisted.revision != state.revision {
            bail!("accounting generation revision changed during shutdown");
        }
        state.revision += 1;
        state.phase = GenerationPhase::Draining;
        state.updated_at = Utc::now().to_rfc3339();
        write_checked(
            &generation_state_path(&self.root, &self.generation_id),
            &*state,
        )?;
        shutdown_remaining(deadline, "publishing final accounting generation state")?;
        coordination.revision += 1;
        replace_coverage(&mut coordination.coverage, coverage(&state));
        write_checked(&self.root.join("coordination.json"), &coordination)?;
        shutdown_remaining(deadline, "publishing final accounting coordination")?;
        File::unlock(&lock).context("unlocking accounting coordination")?;
        Ok(AccountingSnapshot {
            installation_id: coordination.installation_id,
            revision: coordination.revision,
            coverage: coordination.coverage,
        })
    }

    #[cfg(test)]
    fn flush_incident_publications(&self) {
        let target = {
            let _incident = self.incident.lock().expect("accounting incident");
            self.incident_submitted.load(Ordering::Acquire)
        };
        let (completed, changed) = &*self._incident_durable;
        let mut completed = completed.lock().expect("incident publication progress");
        while *completed < target {
            completed = changed
                .wait(completed)
                .expect("incident publication progress");
        }
    }

    fn stop_journal_writer(&self, deadline: Instant) -> Result<()> {
        if self
            .journal_writer
            .lock()
            .expect("accounting journal writer")
            .is_none()
        {
            return Ok(());
        }
        let (completed, receiver) = mpsc::sync_channel(0);
        let mut message = JournalMessage::Shutdown(completed);
        loop {
            match self.journal.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) => {
                    message = returned;
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        bail!("timed out stopping accounting journal writer");
                    }
                    thread::sleep(LOCK_POLL.min(remaining));
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("waiting for accounting journal writer shutdown")?;
        if let Some(handle) = self
            .journal_writer
            .lock()
            .expect("accounting journal writer")
            .take()
        {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("accounting journal writer panicked"))?;
        }
        Ok(())
    }

    fn stop_incident_publisher(&self, deadline: Instant) -> Result<()> {
        self.incident_publisher
            .send(IncidentPublisherMessage::Shutdown)
            .context("stopping accounting incident publisher")?;
        loop {
            let finished = self
                .incident_publisher_thread
                .lock()
                .expect("accounting incident publisher")
                .as_ref()
                .is_none_or(thread::JoinHandle::is_finished);
            if finished {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timed out stopping accounting incident publisher");
            }
            thread::sleep(LOCK_POLL.min(remaining));
        }
        if let Some(handle) = self
            .incident_publisher_thread
            .lock()
            .expect("accounting incident publisher")
            .take()
        {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("accounting incident publisher panicked"))?;
        }
        Ok(())
    }
}

fn shutdown_remaining(deadline: Instant, operation: &str) -> Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("timed out {operation}");
    }
    Ok(remaining)
}

fn ensure_offline(root: &Path) -> Result<AccountingOfflineGuard> {
    let root = resolve_accounting_root(root)?;
    let maintenance = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("maintenance.lock"))?;
    lock_with_timeout(&maintenance, LOCK_TIMEOUT)
        .context("locking accounting offline maintenance lease")?;
    let lock = open_coordination_lock(&root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(&root)?;
    let installation = load_provisioned_root_locked(&root)?;
    let coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    let states = verify_generations_with_adoption(&root, &installation, &coordination, true)?;
    ensure_generations_offline(&root, states.values())?;
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(AccountingOfflineGuard { _file: maintenance })
}

fn acknowledge_incident(
    root: &Path,
    incident_id: &str,
    revision: u64,
    kind: &str,
    evidence_ref: &str,
) -> Result<Value> {
    if evidence_ref.trim().is_empty() {
        bail!("accounting disposition evidence reference must not be empty");
    }
    if !matches!(
        kind,
        "accepted_loss" | "accepted_uncertainty" | "accepted_loss_and_uncertainty"
    ) {
        bail!("unsupported accounting disposition kind {kind}");
    }
    let root = resolve_accounting_root(root)?;
    let lock = open_coordination_lock(&root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(&root)?;
    let installation = load_provisioned_root_locked(&root)?;
    let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    recover_reconciled_coverage_gaps(&root, &mut coordination)?;
    recover_unresolved_coverage_gaps(&root, &mut coordination)?;
    let states = verify_generations_with_adoption(&root, &installation, &coordination, true)?;
    ensure_generations_offline(&root, states.values())?;
    let mut incident: Incident = read_checked(&root.join("incident.json"))?;
    let audit: Vec<AuditRecord> = read_checked(&root.join("audit.json"))?;
    let existing = audit.iter().find(|record| {
        record.operation == "acknowledge"
            && record.incident_id == incident_id
            && record.previous_revision == revision
    });
    if existing
        .filter(|record| {
            incident.state == IncidentState::Acknowledged
                && incident.revision == record.revision
                && incident.disposition == record.disposition
                && record.disposition.as_ref().is_some_and(|disposition| {
                    disposition.kind == kind && disposition.evidence_ref == evidence_ref
                })
        })
        .is_some()
    {
        File::unlock(&lock).context("unlocking accounting coordination")?;
        return Ok(serde_json::to_value(&incident)?);
    }
    if incident.incident_id.as_deref() != Some(incident_id) || incident.revision != revision {
        bail!("accounting incident id or revision changed");
    }
    if !matches!(
        incident.state,
        IncidentState::Blocked | IncidentState::Unreconciled
    ) {
        bail!("accounting incident is not awaiting disposition");
    }
    if has_unresolved_conflict(&root)? {
        bail!("replay content conflicts require verified offline resolution");
    }
    let abandoned_active = abandoned_active_uncertainty(&incident);
    ensure_retained_work_replayed(&root, states.values())?;
    if !abandoned_active
        && (incident.payloads.pending_replay.volatile != Some(0)
            || incident.payloads.pending_replay.durable != Some(0))
    {
        bail!("accounting incident still has pending replay work");
    }
    let lost = incident.payloads.outcome_totals.unrecoverable_lost;
    let unknown = incident.payloads.provisional.unknown;
    let kind_matches = if abandoned_active {
        match lost {
            Some(0) => kind == "accepted_uncertainty",
            Some(_) => kind == "accepted_loss_and_uncertainty",
            None => false,
        }
    } else {
        disposition_matches(kind, lost, unknown)
    };
    if !kind_matches {
        bail!("accounting disposition kind does not match retained loss and uncertainty");
    }

    if let Some(record) = existing {
        let disposition = record
            .disposition
            .as_ref()
            .context("existing acknowledgement audit lacks its disposition")?;
        if record.coverage != coordination.coverage
            || disposition.kind != kind
            || disposition.evidence_ref != evidence_ref
        {
            bail!("existing acknowledgement audit does not match the requested disposition");
        }
        incident.revision = record.revision;
        incident.state = IncidentState::Acknowledged;
        incident.disposition = Some(disposition.clone());
        write_checked(&root.join("incident.json"), &incident)?;
        File::unlock(&lock).context("unlocking accounting coordination")?;
        return Ok(serde_json::to_value(incident)?);
    }

    let actor = effective_actor_identity()?;
    let at = Utc::now().to_rfc3339();
    let disposition = Disposition {
        kind: kind.to_string(),
        actor: actor.clone(),
        at: at.clone(),
        evidence_ref: evidence_ref.to_string(),
    };
    if abandoned_active {
        incident.payloads.pending_replay = PendingReplay {
            volatile: Some(0),
            durable: Some(0),
        };
    }
    incident.revision = incident.revision.saturating_add(1);
    incident.state = IncidentState::Acknowledged;
    incident.disposition = Some(disposition.clone());
    let record = AuditRecord {
        format_version: FORMAT_VERSION,
        installation_id: installation.installation_id,
        operation: "acknowledge".into(),
        incident_id: incident_id.to_string(),
        revision: incident.revision,
        previous_revision: revision,
        coverage: coordination.coverage,
        disposition: Some(disposition),
        event_id: None,
        authoritative: None,
        evidence_ref: evidence_ref.to_string(),
        conflict_snapshot_ref: None,
        conflict_snapshot_digest: None,
        final_row: None,
        actor,
        at,
    };
    append_audit(&root, record)?;
    write_checked(&root.join("incident.json"), &incident)?;
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(serde_json::to_value(incident)?)
}

fn disposition_matches(kind: &str, lost: Option<u64>, unknown: Option<u64>) -> bool {
    match kind {
        "accepted_loss" => lost.is_some_and(|value| value > 0) && unknown == Some(0),
        "accepted_uncertainty" => lost == Some(0) && unknown.is_some_and(|value| value > 0),
        "accepted_loss_and_uncertainty" => {
            lost.is_some_and(|value| value > 0) && unknown.is_some_and(|value| value > 0)
        }
        _ => false,
    }
}

fn abandoned_active_uncertainty(incident: &Incident) -> bool {
    incident.state == IncidentState::Unreconciled
        && incident.cause == Some(IncidentCause::PriorIncidentUnreconciled)
        && incident.payloads.pending_replay.volatile.is_none()
        && incident.payloads.pending_replay.durable.is_none()
        && incident.payloads.provisional.unknown.is_none()
        && incident
            .payloads
            .outcome_totals
            .unrecoverable_lost
            .is_some()
}

fn disposition_matches_incident(kind: &str, incident: &Incident) -> bool {
    let abandoned_kind = match incident.payloads.outcome_totals.unrecoverable_lost {
        Some(0) => "accepted_uncertainty",
        Some(_) => "accepted_loss_and_uncertainty",
        None => "",
    };
    (incident.cause == Some(IncidentCause::PriorIncidentUnreconciled)
        && incident.payloads.provisional.unknown.is_none()
        && kind == abandoned_kind)
        || disposition_matches(
            kind,
            incident.payloads.outcome_totals.unrecoverable_lost,
            incident.payloads.provisional.unknown,
        )
}

#[allow(clippy::too_many_arguments)]
async fn resolve_conflict(
    root: &Path,
    database: &DatabasePool,
    incident_id: &str,
    revision: u64,
    event_id: &str,
    authoritative: &str,
    evidence_ref: &str,
) -> Result<Value> {
    if !matches!(authoritative, "journal" | "database") {
        bail!("conflict authority must be journal or database");
    }
    if evidence_ref.trim().is_empty() {
        bail!("conflict resolution evidence reference must not be empty");
    }
    let root = resolve_accounting_root(root)?;
    let lock = open_coordination_lock(&root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(&root)?;
    let installation = load_provisioned_root_locked(&root)?;
    let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    recover_reconciled_coverage_gaps(&root, &mut coordination)?;
    recover_unresolved_coverage_gaps(&root, &mut coordination)?;
    let states = verify_generations_with_adoption(&root, &installation, &coordination, true)?;
    ensure_generations_offline(&root, states.values())?;
    let mut incident: Incident = read_checked(&root.join("incident.json"))?;
    if incident.incident_id.as_deref() != Some(incident_id) || incident.revision != revision {
        bail!("accounting incident id or revision changed");
    }
    if !matches!(
        incident.state,
        IncidentState::Blocked | IncidentState::Unreconciled
    ) {
        bail!("accounting incident is not awaiting conflict resolution");
    }
    let audits: Vec<AuditRecord> = read_checked(&root.join("audit.json"))?;

    let mut selected: Option<(PathBuf, ReplayManifest, usize, RecordedConflictSnapshot)> = None;
    let mut journal_authority_sides: Option<(ConflictRowEvidence, ConflictRowEvidence)> = None;
    let mut database_authority_row: Option<ConflictRowEvidence> = None;
    for entry in fs::read_dir(root.join("generations"))? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            bail!("generation evidence filename is not valid UTF-8");
        };
        if !name.ends_with(".replay.json") {
            continue;
        }
        let manifest: ReplayManifest = read_checked(&path)?;
        for (index, receipt) in manifest.receipts.iter().enumerate() {
            if receipt.event_id != event_id || receipt.outcome != ReplayOutcome::DuplicateConflict {
                continue;
            }
            let resumable_publication = receipt.resolution_ref.is_some()
                && audits.iter().any(|record| {
                    record.operation == "resolve_conflict"
                        && record.incident_id == incident_id
                        && record.previous_revision == revision
                        && record.event_id.as_deref() == Some(event_id)
                        && receipt.resolution_digest.as_deref()
                            == Some(
                                digest(&serde_json::to_vec(record).unwrap_or_default()).as_str(),
                            )
                });
            if receipt.resolution_ref.is_some() && !resumable_publication {
                continue;
            }
            let snapshot_path = PathBuf::from(
                receipt
                    .conflict_snapshot_ref
                    .as_deref()
                    .context("conflict receipt is missing its protected snapshot")?,
            );
            let snapshot: RecordedConflictSnapshot = read_checked(&snapshot_path)?;
            if receipt.conflict_snapshot_digest.as_deref()
                != Some(digest(&fs::read(&snapshot_path)?).as_str())
                || snapshot.installation_id != installation.installation_id
                || snapshot.generation_id != receipt.generation_id
                || snapshot.journal_offset != receipt.offset
                || snapshot.journal_length != receipt.length
                || snapshot.journal_record_digest != receipt.record_digest
                || snapshot.source.id != event_id
                || snapshot.existing.id != event_id
            {
                bail!("protected conflict evidence does not match its replay receipt");
            }
            validate_same_id_conflict_sides(
                authoritative,
                &snapshot,
                &mut journal_authority_sides,
                &mut database_authority_row,
            )?;
            let replace =
                selected
                    .as_ref()
                    .is_none_or(|(_, selected_manifest, selected_index, _)| {
                        let selected_receipt = &selected_manifest.receipts[*selected_index];
                        (
                            !resumable_publication,
                            receipt.generation_id.as_str(),
                            receipt.offset,
                        ) < (
                            selected_receipt.resolution_ref.is_none(),
                            selected_receipt.generation_id.as_str(),
                            selected_receipt.offset,
                        )
                    });
            if replace {
                selected = Some((path.clone(), manifest.clone(), index, snapshot));
            }
        }
    }
    let (manifest_path, mut manifest, receipt_index, snapshot) =
        selected.context("unresolved conflict receipt was not found")?;
    let journal_bytes = fs::read(generation_journal_path(&root, &snapshot.generation_id))?;
    let parsed = parse_journal(&journal_bytes);
    let frame = parsed
        .frames
        .iter()
        .find(|frame| frame.offset == snapshot.journal_offset)
        .context("protected conflict no longer has its journal frame")?;
    let source_payload: Value = serde_json::from_str(&snapshot.source.payload)
        .context("conflict source payload changed")?;
    if frame.length != snapshot.journal_length
        || frame.digest != snapshot.journal_record_digest
        || frame.event.id != snapshot.source.id
        || frame.event.kind != snapshot.source.kind
        || frame.event.payload != source_payload
        || frame.event.created_at != snapshot.source.created_at
    {
        bail!("protected conflict journal side changed");
    }

    let current = read_background_row(database, event_id).await?;
    let final_row = if authoritative == "journal" {
        if current != snapshot.existing && current != snapshot.source {
            bail!("database conflict side changed before resolution");
        }
        snapshot.source.clone()
    } else {
        if current != snapshot.existing {
            bail!("database conflict side changed before resolution");
        }
        snapshot.existing.clone()
    };

    let next_revision = incident.revision.saturating_add(1);
    let mut resolved_state = states
        .get(&manifest.generation_id)
        .cloned()
        .context("conflict generation state is missing")?;
    let state_already_published = resolved_state.phase == GenerationPhase::Reconciled
        && resolved_state.revision == manifest.source_generation_revision.saturating_add(1);
    if (!state_already_published
        && (resolved_state.phase != GenerationPhase::Adopting
            || resolved_state.revision != manifest.source_generation_revision))
        || manifest.parser_result != "complete"
        || manifest.complete_record_count != manifest.receipts.len() as u64
    {
        bail!("conflict generation is not a complete offline adoption snapshot");
    }
    let generation_resolved = manifest
        .receipts
        .iter()
        .enumerate()
        .all(|(index, receipt)| {
            index == receipt_index
                || receipt.outcome.acknowledges()
                || (receipt.outcome == ReplayOutcome::DuplicateConflict
                    && receipt.resolution_ref.is_some()
                    && receipt.resolution_digest.is_some()
                    && receipt.resolved_authoritative.is_some())
        });
    let mut resolved_coordination = coordination.clone();
    if generation_resolved && !state_already_published {
        resolved_state.revision = resolved_state.revision.saturating_add(1);
        resolved_state.phase = GenerationPhase::Reconciled;
        resolved_state.last_complete_record_boundary = Some(manifest.complete_boundary);
        resolved_state.admitted_count = Some(0);
        resolved_state.worker_completed_count = Some(0);
        resolved_state.journal_synced_count = Some(manifest.complete_record_count);
        resolved_state.database_committed_count = Some(manifest.complete_record_count);
        resolved_state.replay_manifest = Some(manifest_path.display().to_string());
        resolved_state.updated_at = Utc::now().to_rfc3339();
        resolved_coordination.revision = resolved_coordination.revision.saturating_add(1);
        replace_coverage(
            &mut resolved_coordination.coverage,
            coverage(&resolved_state),
        );
    }
    let conflict_snapshot_ref = manifest.receipts[receipt_index]
        .conflict_snapshot_ref
        .clone();
    let conflict_snapshot_digest = manifest.receipts[receipt_index]
        .conflict_snapshot_digest
        .clone();
    let prepared = audits.iter().find(|record| {
        record.operation == "prepare_conflict_resolution"
            && record.incident_id == incident_id
            && record.previous_revision == revision
            && record.event_id.as_deref() == Some(event_id)
    });
    let completed = audits.iter().find(|record| {
        record.operation == "resolve_conflict"
            && record.incident_id == incident_id
            && record.previous_revision == revision
            && record.event_id.as_deref() == Some(event_id)
    });
    let validate_record = |record: &AuditRecord| -> Result<()> {
        if record.revision != next_revision
            || record.coverage != resolved_coordination.coverage
            || record.authoritative.as_deref() != Some(authoritative)
            || record.evidence_ref != evidence_ref
            || record.conflict_snapshot_ref != conflict_snapshot_ref
            || record.conflict_snapshot_digest != conflict_snapshot_digest
            || record.final_row.as_ref() != Some(&final_row)
        {
            bail!("existing conflict resolution audit does not match the requested recovery");
        }
        Ok(())
    };
    let prepared_record = if let Some(record) = prepared {
        validate_record(record)?;
        record.clone()
    } else {
        AuditRecord {
            format_version: FORMAT_VERSION,
            installation_id: installation.installation_id.clone(),
            operation: "prepare_conflict_resolution".into(),
            incident_id: incident_id.to_string(),
            revision: next_revision,
            previous_revision: revision,
            coverage: resolved_coordination.coverage.clone(),
            disposition: None,
            event_id: Some(event_id.to_string()),
            authoritative: Some(authoritative.to_string()),
            evidence_ref: evidence_ref.to_string(),
            conflict_snapshot_ref: conflict_snapshot_ref.clone(),
            conflict_snapshot_digest: conflict_snapshot_digest.clone(),
            final_row: Some(final_row.clone()),
            actor: effective_actor_identity()?,
            at: Utc::now().to_rfc3339(),
        }
    };
    if prepared.is_none() {
        append_audit(&root, prepared_record.clone())?;
    }

    if authoritative == "journal" && current == snapshot.existing {
        replace_background_row(database, &snapshot.existing, &snapshot.source).await?;
    }
    if read_background_row(database, event_id).await? != final_row {
        bail!("database conflict resolution did not verify after mutation");
    }

    let record = if let Some(record) = completed {
        validate_record(record)?;
        record.clone()
    } else {
        let mut record = prepared_record;
        record.operation = "resolve_conflict".into();
        append_audit(&root, record.clone())?;
        record
    };
    let resolution_digest = digest(&serde_json::to_vec(&record)?);
    let receipt = &mut manifest.receipts[receipt_index];
    let resolution_ref =
        format!("audit.json#incident={incident_id}&revision={next_revision}&event={event_id}");
    let receipt_already_published = receipt.resolution_ref.is_some();
    if receipt_already_published {
        if receipt.resolution_ref.as_deref() != Some(resolution_ref.as_str())
            || receipt.resolution_digest.as_deref() != Some(resolution_digest.as_str())
            || receipt.resolved_authoritative.as_deref() != Some(authoritative)
        {
            bail!("published conflict resolution receipt does not match its audit");
        }
    } else {
        receipt.resolution_ref = Some(resolution_ref);
        receipt.resolution_digest = Some(resolution_digest);
        receipt.resolved_authoritative = Some(authoritative.to_string());
        receipt.updated_at = Utc::now().to_rfc3339();
        persist_replay_manifest(&manifest_path, &mut manifest)?;
    }
    if generation_resolved && !state_already_published {
        resolved_state.replay_manifest_digest = Some(digest(&fs::read(&manifest_path)?));
        write_checked(
            &generation_state_path(&root, &resolved_state.generation_id),
            &resolved_state,
        )?;
        write_checked(&root.join("coordination.json"), &resolved_coordination)?;
    }
    if generation_resolved {
        let adoption_path = root.join("adoption.json");
        if adoption_path.exists() {
            let mut adoption: Adoption = read_checked(&adoption_path)?;
            if adoption.state != "complete"
                && adoption.generation_id == resolved_state.generation_id
            {
                finish_reconciled_adoption(&root, &mut adoption, &resolved_state, &manifest_path)?;
            }
        }
    }
    incident.revision = next_revision;
    incident.payloads.pending_replay.durable = incident
        .payloads
        .pending_replay
        .durable
        .map(|pending| pending.saturating_sub(1));
    let clear = all_replay_work_resolved(database, &root).await?
        && incident.payloads.pending_replay.volatile == Some(0)
        && incident.payloads.pending_replay.durable == Some(0)
        && incident.payloads.provisional.unknown == Some(0)
        && incident.payloads.outcome_totals.unrecoverable_lost == Some(0);
    if clear {
        incident.state = IncidentState::Clear;
        incident.incident_id = None;
        incident.first_observed_at = None;
        incident.cause = None;
        incident.disposition = None;
        incident.payloads = clear_incident(&incident.installation_id).payloads;
        incident.publication_ids.clear();
    }
    write_checked(&root.join("incident.json"), &incident)?;
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(serde_json::json!({
        "state":match incident.state {
            IncidentState::Clear => "clear",
            IncidentState::Blocked => "blocked",
            IncidentState::Unreconciled => "unreconciled",
            IncidentState::Acknowledged => "acknowledged",
        },
        "revision":next_revision,
        "authoritative":authoritative,
        "event_id":event_id,
    }))
}

fn validate_same_id_conflict_sides(
    authoritative: &str,
    snapshot: &RecordedConflictSnapshot,
    journal_sides: &mut Option<(ConflictRowEvidence, ConflictRowEvidence)>,
    database_row: &mut Option<ConflictRowEvidence>,
) -> Result<()> {
    if authoritative == "journal" {
        if let Some((source, existing)) = journal_sides.as_ref() {
            if source != &snapshot.source || existing != &snapshot.existing {
                bail!(
                    "same event id has distinct protected conflict sides; journal authority requires disambiguation before mutation"
                );
            }
        } else {
            *journal_sides = Some((snapshot.source.clone(), snapshot.existing.clone()));
        }
    } else if let Some(existing) = database_row.as_ref() {
        if existing != &snapshot.existing {
            bail!(
                "same event id has distinct captured database rows; database authority requires disambiguation before mutation"
            );
        }
    } else {
        *database_row = Some(snapshot.existing.clone());
    }
    Ok(())
}

async fn read_background_row(
    database: &DatabasePool,
    event_id: &str,
) -> Result<ConflictRowEvidence> {
    let row: Option<(String, String, String, String)> =
        match database {
            DatabasePool::Sqlite(pool) => sqlx::query_as(
                "SELECT id, kind, payload, created_at FROM steve_background_events WHERE id = ?",
            )
            .bind(event_id)
            .fetch_optional(pool)
            .await?,
            DatabasePool::Postgres(pool) => sqlx::query_as(
                "SELECT id, kind, payload, created_at FROM steve_background_events WHERE id = $1",
            )
            .bind(event_id)
            .fetch_optional(pool)
            .await?,
        };
    let (id, kind, payload, created_at) = row.context("conflicting database row is missing")?;
    Ok(ConflictRowEvidence {
        id,
        kind,
        payload,
        created_at,
    })
}

async fn replace_background_row(
    database: &DatabasePool,
    expected: &ConflictRowEvidence,
    replacement: &ConflictRowEvidence,
) -> Result<()> {
    let affected = match database {
        DatabasePool::Sqlite(pool) => sqlx::query(
            "UPDATE steve_background_events SET kind = ?, payload = ?, created_at = ?
             WHERE id = ? AND kind = ? AND payload = ? AND created_at = ?",
        )
        .bind(&replacement.kind)
        .bind(&replacement.payload)
        .bind(&replacement.created_at)
        .bind(&expected.id)
        .bind(&expected.kind)
        .bind(&expected.payload)
        .bind(&expected.created_at)
        .execute(pool)
        .await?
        .rows_affected(),
        DatabasePool::Postgres(pool) => sqlx::query(
            "UPDATE steve_background_events SET kind = $1, payload = $2, created_at = $3
             WHERE id = $4 AND kind = $5 AND payload = $6 AND created_at = $7",
        )
        .bind(&replacement.kind)
        .bind(&replacement.payload)
        .bind(&replacement.created_at)
        .bind(&expected.id)
        .bind(&expected.kind)
        .bind(&expected.payload)
        .bind(&expected.created_at)
        .execute(pool)
        .await?
        .rows_affected(),
    };
    if affected != 1 {
        bail!("database conflict side changed during conditional resolution");
    }
    Ok(())
}

async fn all_replay_work_resolved(database: &DatabasePool, root: &Path) -> Result<bool> {
    for entry in fs::read_dir(root.join("generations"))? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".state.json"))
        {
            continue;
        }
        let state: GenerationState = read_checked(&path)?;
        let journal = fs::read(generation_journal_path(root, &state.generation_id))?;
        if state.phase != GenerationPhase::Reconciled
            || journal.len() as u64 != state.journal_length
            || digest(&journal) != state.journal_evidence_digest
        {
            return Ok(false);
        }
        if state.journal_length == 0
            && state.admitted_count == Some(0)
            && state.worker_completed_count == Some(0)
            && state.journal_synced_count == Some(0)
            && state.database_committed_count == Some(0)
        {
            continue;
        }
        let manifest_path = replay_manifest_path(root, &state.generation_id);
        let manifest: ReplayManifest = read_checked(&manifest_path)?;
        let manifest_bytes = fs::read(&manifest_path)?;
        let expected_manifest_path = manifest_path.to_string_lossy();
        let parsed = parse_journal(&journal);
        validate_manifest_summary(&manifest)?;
        if state.replay_manifest.as_deref() != Some(expected_manifest_path.as_ref())
            || state.replay_manifest_digest.as_deref() != Some(digest(&manifest_bytes).as_str())
            || manifest.generation_id != state.generation_id
            || manifest.journal_length != state.journal_length
            || manifest.journal_evidence_digest != state.journal_evidence_digest
            || manifest.parser_result != "complete"
            || manifest.retry_exhausted
            || !parsed.malformed.is_empty()
            || parsed.torn_tail.is_some()
            || parsed.complete_boundary != manifest.complete_boundary
            || parsed.frames.len() as u64 != manifest.complete_record_count
            || manifest.receipts.len() as u64 != manifest.complete_record_count
        {
            return Ok(false);
        }
        for frame in &parsed.frames {
            let Some(receipt) = manifest
                .receipts
                .iter()
                .find(|receipt| receipt.offset == frame.offset)
            else {
                return Ok(false);
            };
            if receipt.length != frame.length
                || receipt.record_digest != frame.digest
                || receipt.event_id != frame.event.id
            {
                return Ok(false);
            }
            if receipt.outcome.acknowledges() {
                continue;
            }
            if receipt.outcome != ReplayOutcome::DuplicateConflict
                || receipt.resolution_ref.is_none()
                || receipt.resolution_digest.is_none()
                || receipt.resolved_authoritative.is_none()
            {
                return Ok(false);
            }
            verify_conflict_resolution(database, root, receipt).await?;
        }
    }
    Ok(true)
}

async fn verify_conflict_resolution(
    database: &DatabasePool,
    root: &Path,
    receipt: &ReplayReceipt,
) -> Result<()> {
    let final_row = verify_conflict_resolution_evidence(root, receipt)?;
    if read_background_row(database, &receipt.event_id).await? != final_row {
        bail!("database conflict resolution no longer matches protected audit evidence");
    }
    Ok(())
}

fn verify_conflict_resolution_evidence(
    root: &Path,
    receipt: &ReplayReceipt,
) -> Result<ConflictRowEvidence> {
    let expected_digest = receipt
        .resolution_digest
        .as_deref()
        .context("conflict resolution digest is missing")?;
    let expected_authority = receipt
        .resolved_authoritative
        .as_deref()
        .context("conflict resolution authority is missing")?;
    let records: Vec<AuditRecord> = read_checked(&root.join("audit.json"))?;
    let mut matched = None;
    for record in &records {
        if record.operation == "resolve_conflict"
            && record.event_id.as_deref() == Some(receipt.event_id.as_str())
            && record.authoritative.as_deref() == Some(expected_authority)
            && digest(&serde_json::to_vec(record)?) == expected_digest
        {
            matched = Some(record);
            break;
        }
    }
    let record = matched.context("conflict resolution audit record is missing or changed")?;
    let expected_ref = format!(
        "audit.json#incident={}&revision={}&event={}",
        record.incident_id, record.revision, receipt.event_id
    );
    if receipt.resolution_ref.as_deref() != Some(expected_ref.as_str()) {
        bail!("conflict resolution reference does not match its audit");
    }
    if record.conflict_snapshot_ref != receipt.conflict_snapshot_ref
        || record.conflict_snapshot_digest != receipt.conflict_snapshot_digest
    {
        bail!("conflict resolution audit does not match protected conflict evidence");
    }
    let final_row = record
        .final_row
        .as_ref()
        .context("conflict resolution audit lacks final database evidence")?;
    Ok(final_row.clone())
}

fn audit_incident(root: &Path, incident_id: &str, revision: u64) -> Result<Value> {
    let root = resolve_accounting_root(root)?;
    let lock = open_coordination_lock(&root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(&root)?;
    let _installation = load_provisioned_root_locked(&root)?;
    let records: Vec<AuditRecord> = read_checked(&root.join("audit.json"))?;
    let record = records
        .into_iter()
        .find(|record| {
            record.incident_id == incident_id
                && record.revision == revision
                && record.operation != "prepare_conflict_resolution"
        })
        .context("accounting audit record was not found")?;
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(serde_json::to_value(record)?)
}

fn append_audit(root: &Path, record: AuditRecord) -> Result<()> {
    let path = root.join("audit.json");
    let mut records: Vec<AuditRecord> = read_checked(&path)?;
    if records.iter().any(|existing| {
        existing.incident_id == record.incident_id
            && existing.revision == record.revision
            && existing.operation == record.operation
    }) {
        bail!("accounting audit revision already exists");
    }
    records.push(record);
    write_checked(&path, &records)
}

fn ensure_generations_offline<'a>(
    root: &Path,
    states: impl Iterator<Item = &'a GenerationState>,
) -> Result<()> {
    for state in states {
        let journal = OpenOptions::new()
            .read(true)
            .write(true)
            .open(generation_journal_path(root, &state.generation_id))?;
        match journal.try_lock() {
            Ok(()) => File::unlock(&journal)?,
            Err(std::fs::TryLockError::WouldBlock) => {
                bail!(
                    "accounting generation {} is still serving",
                    state.generation_id
                )
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).context("checking offline accounting generation")
            }
        }
    }
    Ok(())
}

fn has_unresolved_conflict(root: &Path) -> Result<bool> {
    Ok(unresolved_conflict_count(root)? > 0)
}

fn ensure_retained_work_replayed<'a>(
    root: &Path,
    states: impl Iterator<Item = &'a GenerationState>,
) -> Result<()> {
    for state in states {
        if state.phase == GenerationPhase::Reconciled
            && state.journal_length == 0
            && state.admitted_count == Some(0)
            && state.worker_completed_count == Some(0)
            && state.journal_synced_count == Some(0)
            && state.database_committed_count == Some(0)
        {
            continue;
        }
        let manifest: ReplayManifest =
            read_checked(&replay_manifest_path(root, &state.generation_id)).with_context(|| {
                format!(
                    "generation {} has no verified replay evidence",
                    state.generation_id
                )
            })?;
        let bytes = fs::read(generation_journal_path(root, &state.generation_id))?;
        let parsed = parse_journal(&bytes);
        validate_manifest_summary(&manifest)?;
        let revision_matches = if state.phase == GenerationPhase::Reconciled {
            manifest.source_generation_revision.saturating_add(1) == state.revision
        } else {
            manifest.source_generation_revision == state.revision
        };
        if !revision_matches
            || manifest.generation_id != state.generation_id
            || manifest.journal_length != state.journal_length
            || manifest.journal_evidence_digest != state.journal_evidence_digest
            || manifest.parser_result != "complete"
            || manifest.retry_exhausted
            || !parsed.malformed.is_empty()
            || parsed.torn_tail.is_some()
            || parsed.complete_boundary != manifest.complete_boundary
            || parsed.frames.len() as u64 != manifest.complete_record_count
        {
            bail!(
                "generation {} replay remains unresolved",
                state.generation_id
            );
        }
        for frame in &parsed.frames {
            let receipt = manifest
                .receipts
                .iter()
                .find(|receipt| receipt.offset == frame.offset)
                .context("replay manifest is missing a retained frame receipt")?;
            if receipt.length != frame.length
                || receipt.record_digest != frame.digest
                || receipt.event_id != frame.event.id
                || (!receipt.outcome.acknowledges()
                    && !(receipt.outcome == ReplayOutcome::DuplicateConflict
                        && receipt.resolution_ref.is_some()
                        && receipt.resolution_digest.is_some()
                        && receipt.resolved_authoritative.is_some()))
            {
                bail!(
                    "generation {} replay remains unresolved",
                    state.generation_id
                );
            }
        }
    }
    Ok(())
}

fn provision_root(root: &Path, adoption: Option<AdoptionSeed>) -> Result<()> {
    ensure_platform_supported()?;
    require_absolute(root, "accounting root")?;
    fs::create_dir_all(root)
        .with_context(|| format!("creating accounting root {}", root.display()))?;
    set_private_directory(root)?;
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving accounting root {}", root.display()))?;
    let generations = root.join("generations");
    fs::create_dir_all(&generations)
        .with_context(|| format!("creating {}", generations.display()))?;
    set_private_directory(&generations)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("maintenance.lock"))
        .context("opening accounting maintenance lock")?;

    let coordination_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("coordination.lock"))
        .context("opening accounting coordination lock")?;
    lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
    recover_checked_publications(&root)?;
    verify_root_inventory(&root)?;
    if root.join("provisioning.json").exists() {
        if let Some(seed) = adoption.as_ref() {
            let installation = load_provisioned_root_locked(&root)?;
            let existing: Adoption = read_checked(&root.join("adoption.json"))?;
            if existing.installation_id != installation.installation_id
                || existing.source != seed.source
                || existing.backup != seed.backup
                || existing.source_length != seed.source_length
                || existing.source_digest != seed.source_digest
                || existing.maintenance_assertion != seed.maintenance_assertion
            {
                bail!("existing accounting root has different adoption evidence");
            }
            File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
            return Ok(());
        }
        bail!(
            "accounting root {} already contains ownership evidence",
            root.display()
        );
    }
    if fs::read_dir(&generations)?.next().is_some() {
        bail!("interrupted provisioning has unexpected generation evidence");
    }

    let installation_path = root.join("installation.json");
    let installation = if installation_path.exists() {
        let installation: Installation = read_checked(&installation_path)?;
        if installation.format_version != FORMAT_VERSION
            || installation.root != root.display().to_string()
        {
            bail!("interrupted provisioning installation evidence is inconsistent");
        }
        installation
    } else {
        let installation = Installation {
            format_version: FORMAT_VERSION,
            installation_id: Uuid::now_v7().to_string(),
            root: root.display().to_string(),
        };
        write_checked(&installation_path, &installation)?;
        installation
    };
    let installation_id = &installation.installation_id;

    let incident_path = root.join("incident.json");
    if incident_path.exists() {
        let incident: Incident = read_checked(&incident_path)?;
        if incident.installation_id != *installation_id {
            bail!("interrupted provisioning incident identity is inconsistent");
        }
        validate_incident(&incident)?;
    } else {
        write_checked(&incident_path, &clear_incident(installation_id))?;
    }

    let audit_path = root.join("audit.json");
    if audit_path.exists() {
        let records: Vec<AuditRecord> = read_checked(&audit_path)?;
        if records
            .iter()
            .any(|record| record.installation_id != *installation_id)
        {
            bail!("interrupted provisioning audit identity is inconsistent");
        }
    } else {
        write_checked(&audit_path, &Vec::<AuditRecord>::new())?;
    }

    let coordination_path = root.join("coordination.json");
    if coordination_path.exists() {
        let coordination: Coordination = read_checked(&coordination_path)?;
        if coordination.format_version != FORMAT_VERSION
            || coordination.installation_id != *installation_id
            || coordination.revision != 1
            || !coordination.coverage.is_empty()
        {
            bail!("interrupted provisioning coordination evidence is inconsistent");
        }
    } else {
        write_checked(
            &coordination_path,
            &Coordination {
                format_version: FORMAT_VERSION,
                installation_id: installation_id.clone(),
                revision: 1,
                coverage: Vec::new(),
            },
        )?;
    }

    let adoption_path = root.join("adoption.json");
    match adoption {
        Some(seed) if adoption_path.exists() => {
            let existing: Adoption = read_checked(&adoption_path)?;
            if existing.format_version != FORMAT_VERSION
                || existing.installation_id != *installation_id
                || existing.source != seed.source
                || existing.backup != seed.backup
                || existing.source_length != seed.source_length
                || existing.source_digest != seed.source_digest
                || existing.maintenance_assertion != seed.maintenance_assertion
            {
                bail!("interrupted legacy adoption evidence is inconsistent");
            }
        }
        Some(seed) => write_checked(
            &adoption_path,
            &Adoption {
                format_version: FORMAT_VERSION,
                installation_id: installation_id.clone(),
                state: seed.state,
                source: seed.source,
                backup: seed.backup,
                source_length: seed.source_length,
                source_digest: seed.source_digest,
                maintenance_assertion: seed.maintenance_assertion,
                generation_id: seed.generation_id,
                record_outcome: seed.record_outcome,
                accepted_evidence_ref: None,
                retained_source: None,
                source_directory_synced: false,
                updated_at: Utc::now().to_rfc3339(),
            },
        )?,
        None if adoption_path.exists() => {
            bail!("interrupted provisioning unexpectedly contains adoption evidence")
        }
        None => {}
    }

    sync_directory(&root)?;
    if let Some(parent) = root.parent() {
        sync_directory(parent)?;
    }
    let operator = operator_identity()?;
    write_checked(
        &root.join("provisioning.json"),
        &Provisioning {
            format_version: FORMAT_VERSION,
            installation_id: installation_id.clone(),
            operator,
            root: root.display().to_string(),
            inventory: Vec::new(),
            revision: 1,
            created_at: Utc::now().to_rfc3339(),
            files_synced: true,
            directory_synced: true,
        },
    )?;
    File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
    Ok(())
}

fn adopt_legacy(source: &Path, root: &Path, maintenance_assertion: &Path) -> Result<String> {
    ensure_platform_supported()?;
    require_absolute(source, "legacy accounting journal")?;
    require_absolute(root, "accounting root")?;
    require_absolute(maintenance_assertion, "maintenance assertion")?;
    if source == root || source.starts_with(root) || root.starts_with(source) {
        bail!("legacy source and accounting root must be distinct paths");
    }
    if !source.exists() {
        let source = canonicalize_missing_path(source)?;
        return resume_empty_adoption_after_move(&source, root, maintenance_assertion);
    }

    let source_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(source)
        .with_context(|| format!("opening legacy journal {}", source.display()))?;
    if !source_file.metadata()?.is_file() {
        bail!("legacy journal {} is not a regular file", source.display());
    }
    lock_with_timeout(&source_file, LOCK_TIMEOUT).with_context(|| {
        format!(
            "legacy journal {} is busy; the guard covers cooperating writers only",
            source.display()
        )
    })?;

    let source = fs::canonicalize(source)
        .with_context(|| format!("resolving legacy journal {}", source.display()))?;
    let bytes = fs::read(&source)
        .with_context(|| format!("reading legacy journal {}", source.display()))?;
    let assertion = read_maintenance_assertion(maintenance_assertion, &source)?;
    let source_digest = digest(&bytes);
    let backup = source.with_file_name(format!(
        "{}.steve-backup",
        source
            .file_name()
            .and_then(|name| name.to_str())
            .context("legacy journal name is not valid UTF-8")?
    ));
    write_or_verify_backup(&backup, &bytes)?;
    ensure_source_unchanged(&source, &bytes)?;

    provision_root(
        root,
        Some(AdoptionSeed {
            state: "importing".into(),
            source: source.display().to_string(),
            backup: backup.display().to_string(),
            source_length: bytes.len() as u64,
            source_digest: source_digest.clone(),
            maintenance_assertion: assertion.clone(),
            generation_id: Uuid::now_v7().to_string(),
            record_outcome: "pending".into(),
        }),
    )?;

    let root = resolve_accounting_root(root)?;
    let coordination_lock = open_coordination_lock(&root)?;
    lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
    recover_checked_publications(&root)?;
    let installation = load_provisioned_root_locked(&root)?;
    let adoption_path = root.join("adoption.json");
    if !adoption_path.exists() {
        bail!("accounting root is missing legacy adoption evidence");
    }
    let mut adoption: Adoption = read_checked(&adoption_path)?;
    ensure_adoption_matches(
        &adoption,
        &installation,
        &source,
        &backup,
        &bytes,
        &assertion,
    )?;

    let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    let journal_path = generation_journal_path(&root, &adoption.generation_id);
    let importing_path = journal_path.with_extension("journal.importing");
    let state_path = generation_state_path(&root, &adoption.generation_id);
    let journal = open_or_publish_import(&root, &journal_path, &importing_path, &bytes)?;
    let mut state = if state_path.exists() {
        let state: GenerationState = read_checked(&state_path)?;
        verify_import_state(&state, &installation, &adoption, &journal_path, &bytes)?;
        state
    } else {
        let state = GenerationState {
            format_version: FORMAT_VERSION,
            installation_id: installation.installation_id.clone(),
            generation_id: adoption.generation_id.clone(),
            revision: 1,
            phase: if bytes.is_empty() {
                GenerationPhase::Reconciled
            } else {
                GenerationPhase::Adopting
            },
            journal: journal_path.display().to_string(),
            journal_length: bytes.len() as u64,
            journal_evidence_digest: source_digest,
            admitted_count: Some(0),
            worker_completed_count: Some(0),
            journal_synced_count: Some(parse_journal(&bytes).frames.len() as u64),
            database_committed_count: Some(0),
            last_complete_record_boundary: bytes.is_empty().then_some(0),
            replay_manifest: None,
            replay_manifest_digest: None,
            updated_at: Utc::now().to_rfc3339(),
        };
        write_checked(&state_path, &state)?;
        state
    };
    let expected_coverage = coverage(&state);
    match coordination
        .coverage
        .iter()
        .find(|entry| entry.generation_id == adoption.generation_id)
    {
        Some(existing) if existing != &expected_coverage => {
            bail!("legacy adoption coverage conflicts with generation evidence")
        }
        Some(_) => {}
        None => {
            coordination.revision += 1;
            coordination.coverage.push(expected_coverage);
            sort_coverage(&mut coordination.coverage);
            write_checked(&root.join("coordination.json"), &coordination)?;
        }
    }

    ensure_source_unchanged(&source, &bytes)?;
    if bytes.is_empty() {
        let manifest_path = replay_manifest_path(&root, &adoption.generation_id);
        let parsed = parse_journal(&bytes);
        let _manifest = load_or_create_replay_manifest(
            &manifest_path,
            &installation,
            &coordination,
            &state,
            &parsed,
            serde_json::json!({"backend":"offline_adoption"}),
        )?;
        let manifest_digest = digest(&fs::read(&manifest_path)?);
        state.revision += 1;
        state.last_complete_record_boundary = Some(0);
        state.replay_manifest = Some(manifest_path.display().to_string());
        state.replay_manifest_digest = Some(manifest_digest.clone());
        state.updated_at = Utc::now().to_rfc3339();
        write_checked(&state_path, &state)?;
        coordination.revision += 1;
        replace_coverage(&mut coordination.coverage, coverage(&state));
        write_checked(&root.join("coordination.json"), &coordination)?;
        let retained = retained_source_path(&source)?;
        adoption.state = "retaining_source".into();
        adoption.record_outcome = "empty_vacuous".into();
        adoption.accepted_evidence_ref = Some(format!("sha256:{manifest_digest}"));
        adoption.retained_source = Some(retained.display().to_string());
        adoption.source_directory_synced = false;
        adoption.updated_at = Utc::now().to_rfc3339();
        write_checked(&adoption_path, &adoption)?;
        ensure_source_unchanged(&source, &bytes)?;
        if retained.exists() {
            bail!(
                "retained legacy source already exists at {}",
                retained.display()
            );
        }
        fs::rename(&source, &retained).context("retaining empty legacy source")?;
        sync_directory(source.parent().context("legacy source has no parent")?)?;
        adoption.state = "complete".into();
        adoption.source_directory_synced = true;
        adoption.updated_at = Utc::now().to_rfc3339();
        write_checked(&adoption_path, &adoption)?;
        File::unlock(&journal).context("unlocking imported generation")?;
        File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
        File::unlock(&source_file).context("unlocking legacy journal")?;
        return Ok(format!(
            "empty legacy accounting journal adopted for generation {}",
            adoption.generation_id
        ));
    }
    adoption.state = "pending_reconciliation".into();
    adoption.record_outcome = "pending_reconciliation".into();
    adoption.updated_at = Utc::now().to_rfc3339();
    write_checked(&adoption_path, &adoption)?;
    File::unlock(&journal).context("unlocking imported generation")?;
    File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
    File::unlock(&source_file).context("unlocking legacy journal")?;
    Ok(format!(
        "legacy accounting journal imported; pending reconciliation for generation {}",
        adoption.generation_id
    ))
}

async fn reconcile_startup(
    root: &Path,
    database: &DatabasePool,
    operation_timeout: Duration,
    retry_deadline: Duration,
    retry_interval: Duration,
    restore_prior_incident: bool,
) -> Result<()> {
    let policy = ReplayPolicy {
        operation_timeout,
        retry_deadline,
        retry_interval,
    };
    let database_durability = serde_json::to_value(database.durability_profile().await?)?;
    let root = resolve_accounting_root(root)?;
    let coordination_lock = open_coordination_lock(&root)?;
    lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
    recover_checked_publications(&root)?;
    let installation = load_provisioned_root_locked(&root)?;
    let mut incident: Incident = read_checked(&root.join("incident.json"))?;
    let mut acknowledgement = None;
    if incident.state == IncidentState::Acknowledged {
        let audit: Vec<AuditRecord> = read_checked(&root.join("audit.json"))?;
        acknowledgement = audit.into_iter().find(|record| {
            record.operation == "acknowledge"
                && Some(record.incident_id.as_str()) == incident.incident_id.as_deref()
                && record.revision == incident.revision
                && record.disposition == incident.disposition
        });
        if acknowledgement.is_none() {
            incident.revision = incident.revision.saturating_add(1);
            incident.state = IncidentState::Unreconciled;
            incident.cause = Some(IncidentCause::PriorIncidentUnreconciled);
            incident.disposition = None;
            write_checked(&root.join("incident.json"), &incident)?;
        }
    }
    if restore_prior_incident && incident.state == IncidentState::Blocked {
        incident.revision = incident.revision.saturating_add(1);
        incident.state = IncidentState::Unreconciled;
        incident.cause = Some(IncidentCause::PriorIncidentUnreconciled);
        write_checked(&root.join("incident.json"), &incident)?;
    }
    cleanup_empty_staging(&root)?;
    let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    recover_reconciled_coverage_gaps(&root, &mut coordination)?;
    if incident.state != IncidentState::Clear {
        recover_unresolved_coverage_gaps(&root, &mut coordination)?;
    }

    let adoption_path = root.join("adoption.json");
    let adoption_record = adoption_path
        .exists()
        .then(|| read_checked::<Adoption>(&adoption_path))
        .transpose()?;
    if let Some(complete) = adoption_record
        .as_ref()
        .filter(|adoption| adoption.state == "complete")
    {
        validate_complete_adoption(&root, &installation, complete)?;
    }
    let mut adoption = adoption_record.filter(|adoption| adoption.state != "complete");
    let states = verify_generations_with_adoption(&root, &installation, &coordination, true)?;
    if let Some(record) = acknowledgement.as_ref() {
        if validate_acknowledgement_binding(&root, record, &incident, &states).is_err() {
            incident.revision = incident.revision.saturating_add(1);
            incident.state = IncidentState::Unreconciled;
            incident.cause = Some(IncidentCause::PriorIncidentUnreconciled);
            incident.disposition = None;
            write_checked(&root.join("incident.json"), &incident)?;
            acknowledgement = None;
        }
    }
    if let Some(pending) = adoption.as_mut() {
        let state = states
            .get(&pending.generation_id)
            .context("pending adoption is missing generation state")?;
        if state.phase == GenerationPhase::Reconciled {
            finish_reconciled_adoption(
                &root,
                pending,
                state,
                &replay_manifest_path(&root, &state.generation_id),
            )?;
        } else if state.phase != GenerationPhase::Adopting {
            bail!("pending adoption generation is not available for replay");
        }
    }
    File::unlock(&coordination_lock).context("unlocking accounting coordination")?;

    for state in states.values() {
        if state.phase == GenerationPhase::Reconciled {
            if let Some(path) = state.replay_manifest.as_deref() {
                let manifest: ReplayManifest = read_checked(Path::new(path))?;
                let mut invalid = false;
                for receipt in manifest
                    .receipts
                    .iter()
                    .filter(|receipt| receipt.resolution_ref.is_some())
                {
                    if verify_conflict_resolution(database, &root, receipt)
                        .await
                        .is_err()
                    {
                        invalid = true;
                        break;
                    }
                }
                if invalid {
                    publish_unreconciled(&root)?;
                }
            }
            continue;
        }
        if !matches!(
            state.phase,
            GenerationPhase::Active | GenerationPhase::Draining | GenerationPhase::Adopting
        ) {
            continue;
        }
        let adoption_for_generation = adoption
            .as_mut()
            .filter(|pending| pending.generation_id == state.generation_id);
        if state.phase == GenerationPhase::Adopting && adoption_for_generation.is_none() {
            bail!("adopting generation has no pending adoption evidence");
        }
        let result = reconcile_generation(
            &root,
            database,
            &installation,
            state,
            database_durability.clone(),
            policy,
            adoption_for_generation,
        )
        .await;
        if let Err(error) = result {
            let current_incident: Incident = read_checked(&root.join("incident.json"))?;
            if state.phase == GenerationPhase::Active
                && current_incident.state != IncidentState::Acknowledged
            {
                publish_abandoned_active(&root, state)?;
                tracing::warn!(%error, generation_id = %state.generation_id, "converted abandoned active generation into an operator-disposable uncertainty incident");
                continue;
            }
            let accepted_coverage = acknowledgement.as_ref().is_some_and(|record| {
                current_incident.state == IncidentState::Acknowledged
                    && record.coverage.contains(&coverage(state))
            });
            if current_incident.state == IncidentState::Acknowledged && !accepted_coverage {
                publish_unreconciled(&root)?;
                tracing::warn!(%error, generation_id = %state.generation_id, "new unresolved generation is outside acknowledged accounting coverage");
                continue;
            }
            if matches!(
                current_incident.state,
                IncidentState::Blocked | IncidentState::Unreconciled
            ) || accepted_coverage
            {
                tracing::warn!(%error, generation_id = %state.generation_id, "retaining unresolved incident generation for offline recovery");
                continue;
            }
            return Err(error);
        }
    }
    Ok(())
}

async fn reconcile_generation(
    root: &Path,
    database: &DatabasePool,
    installation: &Installation,
    state: &GenerationState,
    database_durability: Value,
    policy: ReplayPolicy,
    mut adoption: Option<&mut Adoption>,
) -> Result<()> {
    let journal_path = generation_journal_path(root, &state.generation_id);
    let journal = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&journal_path)
        .context("opening journal for replay")?;
    match journal.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) if state.phase == GenerationPhase::Active => {
            return Ok(())
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            bail!("adopting generation journal is still busy")
        }
        Err(std::fs::TryLockError::Error(err)) => {
            return Err(err).context("locking journal for replay")
        }
    }

    let coordination_lock = open_coordination_lock(root)?;
    lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    let coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    let current: GenerationState =
        read_checked(&generation_state_path(root, &state.generation_id))?;
    if current.revision != state.revision
        || current.phase != state.phase
        || coordination
            .coverage
            .iter()
            .find(|entry| entry.generation_id == state.generation_id)
            != Some(&coverage(state))
    {
        bail!("accounting generation changed before replay snapshot");
    }
    let bytes = fs::read(&journal_path)?;
    if bytes.len() as u64 != state.journal_length || digest(&bytes) != state.journal_evidence_digest
    {
        bail!("journal no longer matches ownership evidence");
    }
    if let Some(pending) = adoption.as_deref() {
        if bytes.len() as u64 != pending.source_length || digest(&bytes) != pending.source_digest {
            bail!("adopted journal no longer matches ownership evidence");
        }
        verify_adoption_copy(pending, true)?;
    }

    let parsed = parse_journal(&bytes);
    let source_coverage = coverage(state);
    let manifest_path = replay_manifest_path(root, &state.generation_id);
    let mut manifest = load_or_create_replay_manifest(
        &manifest_path,
        installation,
        &coordination,
        state,
        &parsed,
        database_durability,
    )?;
    File::unlock(&coordination_lock).context("unlocking accounting coordination")?;

    replay_frames(
        database,
        root,
        installation,
        &parsed,
        &manifest_path,
        &mut manifest,
        policy,
    )
    .await?;

    let complete = parsed.malformed.is_empty()
        && parsed.torn_tail.is_none()
        && parsed.frames.iter().all(|frame| {
            manifest
                .receipts
                .iter()
                .find(|receipt| receipt.offset == frame.offset)
                .is_some_and(|receipt| {
                    receipt.record_digest == frame.digest
                        && (receipt.outcome.acknowledges()
                            || (receipt.outcome == ReplayOutcome::DuplicateConflict
                                && receipt.resolution_ref.is_some()
                                && receipt.resolution_digest.is_some()
                                && receipt.resolved_authoritative.is_some()))
                })
        });
    if !complete {
        File::unlock(&journal).context("unlocking journal")?;
        bail!("accounting replay remains unresolved");
    }
    if state.phase == GenerationPhase::Active {
        File::unlock(&journal).context("unlocking journal")?;
        bail!("prior active accounting generation has no verified completion boundary");
    }

    let coordination_lock = open_coordination_lock(root)?;
    lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    let mut current: GenerationState =
        read_checked(&generation_state_path(root, &state.generation_id))?;
    if coordination.revision != manifest.source_coordination_revision
        || current.revision != manifest.source_generation_revision
        || current.phase != state.phase
        || coordination
            .coverage
            .iter()
            .find(|entry| entry.generation_id == state.generation_id)
            != Some(&source_coverage)
        || fs::read(&journal_path)? != bytes
    {
        bail!("accounting replay snapshot changed before publication");
    }
    let manifest_digest = digest(&fs::read(&manifest_path)?);
    current.revision += 1;
    current.phase = GenerationPhase::Reconciled;
    current.last_complete_record_boundary = Some(parsed.complete_boundary);
    if state.phase == GenerationPhase::Adopting {
        current.admitted_count = Some(0);
        current.worker_completed_count = Some(0);
        current.journal_synced_count = Some(parsed.frames.len() as u64);
        current.database_committed_count = Some(
            manifest
                .receipts
                .iter()
                .filter(|receipt| receipt.outcome.acknowledges())
                .count() as u64,
        );
    }
    current.replay_manifest = Some(manifest_path.display().to_string());
    current.replay_manifest_digest = Some(manifest_digest);
    current.updated_at = Utc::now().to_rfc3339();
    write_checked(
        &generation_state_path(root, &current.generation_id),
        &current,
    )?;
    coordination.revision += 1;
    replace_coverage(&mut coordination.coverage, coverage(&current));
    write_checked(&root.join("coordination.json"), &coordination)?;
    if let Some(pending) = adoption.as_mut() {
        finish_reconciled_adoption(root, pending, &current, &manifest_path)?;
    }
    File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
    File::unlock(&journal).context("unlocking journal")?;
    Ok(())
}

fn load_or_create_replay_manifest(
    path: &Path,
    installation: &Installation,
    coordination: &Coordination,
    state: &GenerationState,
    parsed: &ParsedJournal,
    database_durability: Value,
) -> Result<ReplayManifest> {
    let tail_offset = parsed.torn_tail.as_ref().map(|tail| tail.offset);
    let tail_length = parsed.torn_tail.as_ref().map_or(0, |tail| tail.length);
    let tail_digest = parsed.torn_tail.as_ref().map(|tail| tail.digest.clone());
    if path.exists() {
        let mut manifest: ReplayManifest = read_checked(path)?;
        if manifest.format_version != FORMAT_VERSION
            || manifest.installation_id != installation.installation_id
            || manifest.generation_id != state.generation_id
            || manifest.source_coordination_revision != coordination.revision
            || manifest.source_generation_revision != state.revision
            || manifest.journal_length != state.journal_length
            || manifest.journal_evidence_digest != state.journal_evidence_digest
            || manifest.complete_boundary != parsed.complete_boundary
            || manifest.complete_record_count != parsed.frames.len() as u64
            || manifest.malformed_frames != parsed.malformed
            || manifest.torn_tail_offset != tail_offset
            || manifest.torn_tail_length != tail_length
            || manifest.torn_tail_digest != tail_digest
            || manifest.parser_result != parser_result(parsed)
            || manifest.database_durability != database_durability
        {
            bail!("replay manifest does not match frozen journal evidence");
        }
        validate_manifest_summary(&manifest)?;
        let mut receipt_offsets = BTreeSet::new();
        for receipt in &manifest.receipts {
            if !receipt_offsets.insert(receipt.offset) {
                bail!("replay manifest contains duplicate frame receipts");
            }
            let Some(frame) = parsed
                .frames
                .iter()
                .find(|frame| frame.offset == receipt.offset)
            else {
                bail!("replay manifest contains an unknown frame receipt");
            };
            if receipt.length != frame.length
                || receipt.generation_id != manifest.generation_id
                || receipt.record_digest != frame.digest
                || receipt.event_id != frame.event.id
                || receipt.content_digest != event_content_digest(&frame.event)?
            {
                bail!("replay receipt does not match frozen journal frame");
            }
            if receipt.outcome.acknowledges()
                && (receipt.database_evidence_ref.is_none()
                    || receipt.database_evidence_at.is_none())
            {
                bail!("acknowledged replay receipt lacks database evidence");
            }
        }
        let root = path
            .parent()
            .context("manifest has no parent")?
            .parent()
            .context("generations has no parent")?;
        if recover_conflict_receipts(root, installation, parsed, &mut manifest)? {
            persist_replay_manifest(path, &mut manifest)?;
        }
        return Ok(manifest);
    }
    let mut manifest = ReplayManifest {
        format_version: FORMAT_VERSION,
        installation_id: installation.installation_id.clone(),
        generation_id: state.generation_id.clone(),
        revision: 1,
        source_coordination_revision: coordination.revision,
        source_generation_revision: state.revision,
        journal_length: state.journal_length,
        journal_evidence_digest: state.journal_evidence_digest.clone(),
        complete_boundary: parsed.complete_boundary,
        complete_record_count: parsed.frames.len() as u64,
        malformed_frames: parsed.malformed.clone(),
        torn_tail_offset: tail_offset,
        torn_tail_length: tail_length,
        torn_tail_digest: tail_digest,
        database_durability,
        receipts: Vec::new(),
        ordered_receipt_digest: String::new(),
        outcome_counts: BTreeMap::new(),
        parser_result: parser_result(parsed).into(),
        sync_result: "synced".into(),
        retry_exhausted: false,
        updated_at: Utc::now().to_rfc3339(),
    };
    refresh_manifest_summary(&mut manifest)?;
    write_checked(path, &manifest)?;
    Ok(manifest)
}

fn recover_conflict_receipts(
    root: &Path,
    installation: &Installation,
    parsed: &ParsedJournal,
    manifest: &mut ReplayManifest,
) -> Result<bool> {
    let mut recovered = BTreeMap::new();
    for entry in fs::read_dir(root.join("generations"))? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            bail!("generation conflict filename is not valid UTF-8");
        };
        let Some((generation_id, artifact_offset, artifact_length, artifact_digest)) =
            conflict_snapshot_identity(name)
        else {
            continue;
        };
        if generation_id != manifest.generation_id {
            continue;
        }

        let snapshot: RecordedConflictSnapshot = read_checked(&path)?;
        let frame = parsed
            .frames
            .iter()
            .find(|frame| frame.offset == snapshot.journal_offset)
            .context("conflict evidence has no matching frozen journal frame")?;
        let expected_source = ConflictRowEvidence {
            id: frame.event.id.clone(),
            kind: frame.event.kind.clone(),
            payload: frame.event.payload.to_string(),
            created_at: frame.event.created_at.clone(),
        };
        let expected_path = conflict_snapshot_path(
            root,
            generation_id,
            frame.offset,
            frame.length,
            &frame.digest,
        );
        let durability_backend = manifest
            .database_durability
            .get("backend")
            .and_then(Value::as_str)
            .context("replay manifest durability profile has no backend")?;
        if path != expected_path
            || artifact_offset != frame.offset
            || artifact_length != frame.length
            || artifact_digest != frame.digest
            || snapshot.format_version != FORMAT_VERSION
            || snapshot.installation_id != installation.installation_id
            || snapshot.generation_id != manifest.generation_id
            || snapshot.journal_length != frame.length
            || snapshot.journal_record_digest != frame.digest
            || snapshot.source != expected_source
            || snapshot.existing.id != frame.event.id
            || snapshot.existing_row_digest != digest(&serde_json::to_vec(&snapshot.existing)?)
            || snapshot.verification_id.is_empty()
            || snapshot.backend != durability_backend
            || snapshot.transaction_isolation.is_empty()
            || snapshot.read_evidence
                != format!(
                    "{}:{}:{}",
                    snapshot.backend, snapshot.transaction_isolation, snapshot.verification_id
                )
            || snapshot.observed_at.is_empty()
        {
            bail!("conflict evidence does not match frozen journal frame");
        }
        let snapshot_digest = digest(&fs::read(&path)?);
        let prior_attempts = manifest
            .receipts
            .iter()
            .find(|receipt| receipt.offset == frame.offset)
            .map_or(0, |receipt| receipt.attempts);
        let receipt = ReplayReceipt {
            generation_id: manifest.generation_id.clone(),
            offset: frame.offset,
            length: frame.length,
            record_digest: frame.digest.clone(),
            event_id: frame.event.id.clone(),
            content_digest: event_content_digest(&frame.event)?,
            outcome: ReplayOutcome::DuplicateConflict,
            attempts: prior_attempts.saturating_add(1),
            error: None,
            database_evidence_ref: Some(snapshot.verification_id),
            database_evidence_at: Some(snapshot.observed_at.clone()),
            conflict_snapshot_ref: Some(path.display().to_string()),
            conflict_snapshot_digest: Some(snapshot_digest),
            resolution_ref: None,
            resolution_digest: None,
            resolved_authoritative: None,
            updated_at: snapshot.observed_at,
        };
        if recovered.insert(frame.offset, receipt).is_some() {
            bail!("multiple conflict artifacts map to one journal frame");
        }
    }

    for receipt in &manifest.receipts {
        if receipt.outcome == ReplayOutcome::DuplicateConflict
            && !recovered.contains_key(&receipt.offset)
        {
            bail!("conflict receipt is missing protected evidence");
        }
    }

    let mut changed = false;
    for (offset, recovered_receipt) in recovered {
        match manifest
            .receipts
            .iter_mut()
            .find(|receipt| receipt.offset == offset)
        {
            Some(receipt) if receipt.outcome == ReplayOutcome::DuplicateConflict => {
                if receipt.length != recovered_receipt.length
                    || receipt.record_digest != recovered_receipt.record_digest
                    || receipt.event_id != recovered_receipt.event_id
                    || receipt.content_digest != recovered_receipt.content_digest
                    || receipt.database_evidence_ref != recovered_receipt.database_evidence_ref
                    || receipt.conflict_snapshot_ref != recovered_receipt.conflict_snapshot_ref
                    || receipt.conflict_snapshot_digest
                        != recovered_receipt.conflict_snapshot_digest
                {
                    bail!("conflict receipt does not match protected evidence");
                }
            }
            Some(receipt) => {
                *receipt = recovered_receipt;
                changed = true;
            }
            None => {
                manifest.receipts.push(recovered_receipt);
                changed = true;
            }
        }
    }
    Ok(changed)
}

async fn replay_frames(
    database: &DatabasePool,
    root: &Path,
    installation: &Installation,
    parsed: &ParsedJournal,
    manifest_path: &Path,
    manifest: &mut ReplayManifest,
    policy: ReplayPolicy,
) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(policy.retry_deadline)
        .context("accounting replay retry deadline is out of range")?;
    manifest.retry_exhausted = false;

    for frame in &parsed.frames {
        let receipt_index = match manifest
            .receipts
            .iter()
            .position(|receipt| receipt.offset == frame.offset)
        {
            Some(index) => index,
            None => {
                manifest.receipts.push(ReplayReceipt {
                    generation_id: manifest.generation_id.clone(),
                    offset: frame.offset,
                    length: frame.length,
                    record_digest: frame.digest.clone(),
                    event_id: frame.event.id.clone(),
                    content_digest: event_content_digest(&frame.event)?,
                    outcome: ReplayOutcome::Unknown,
                    attempts: 0,
                    error: Some("not attempted".into()),
                    database_evidence_ref: None,
                    database_evidence_at: None,
                    conflict_snapshot_ref: None,
                    conflict_snapshot_digest: None,
                    resolution_ref: None,
                    resolution_digest: None,
                    resolved_authoritative: None,
                    updated_at: Utc::now().to_rfc3339(),
                });
                manifest.receipts.len() - 1
            }
        };
        if manifest.receipts[receipt_index].outcome.acknowledges() {
            continue;
        }
        if manifest.receipts[receipt_index].outcome == ReplayOutcome::DuplicateConflict {
            if manifest.receipts[receipt_index].resolution_ref.is_some() {
                verify_conflict_resolution(database, root, &manifest.receipts[receipt_index])
                    .await?;
            } else {
                latch_replay_conflict(root, installation)?;
            }
            continue;
        }

        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                let receipt = &mut manifest.receipts[receipt_index];
                receipt.outcome = ReplayOutcome::Unknown;
                receipt.error = Some("accounting replay retry deadline exhausted".into());
                receipt.updated_at = Utc::now().to_rfc3339();
                manifest.retry_exhausted = true;
                publish_replay_manifest(root, manifest_path, manifest)?;
                break;
            };
            let bound = policy.operation_timeout.min(remaining);
            let result = tokio::time::timeout(
                bound,
                database.insert_background_event(
                    &frame.event.id,
                    &frame.event.kind,
                    &frame.event.payload.to_string(),
                    &frame.event.created_at,
                ),
            )
            .await;
            let attempts = manifest.receipts[receipt_index].attempts + 1;
            let receipt = match result {
                Ok(InsertBackgroundEvent::Inserted) => successful_receipt(
                    &manifest.generation_id,
                    frame,
                    ReplayOutcome::Inserted,
                    attempts,
                    &manifest.database_durability,
                )?,
                Ok(InsertBackgroundEvent::DuplicateIdentical) => successful_receipt(
                    &manifest.generation_id,
                    frame,
                    ReplayOutcome::DuplicateIdentical,
                    attempts,
                    &manifest.database_durability,
                )?,
                Ok(InsertBackgroundEvent::DuplicateConflict {
                    existing,
                    verification_id,
                    backend,
                    transaction_isolation,
                }) => conflict_receipt(
                    root,
                    installation,
                    manifest,
                    frame,
                    attempts,
                    existing,
                    &verification_id,
                    &backend,
                    &transaction_isolation,
                )?,
                Ok(InsertBackgroundEvent::Failed { error }) => unresolved_receipt(
                    &manifest.generation_id,
                    frame,
                    ReplayOutcome::Failed,
                    attempts,
                    error,
                )?,
                Ok(InsertBackgroundEvent::Unknown { error }) => unresolved_receipt(
                    &manifest.generation_id,
                    frame,
                    ReplayOutcome::Unknown,
                    attempts,
                    error,
                )?,
                Err(_) => unresolved_receipt(
                    &manifest.generation_id,
                    frame,
                    ReplayOutcome::Unknown,
                    attempts,
                    format!("database operation exceeded {bound:?}"),
                )?,
            };
            let acknowledged = receipt.outcome.acknowledges();
            let conflict = receipt.outcome == ReplayOutcome::DuplicateConflict;
            manifest.receipts[receipt_index] = receipt;
            publish_replay_manifest(root, manifest_path, manifest)?;
            if conflict {
                latch_replay_conflict(root, installation)?;
            }
            if acknowledged || conflict {
                break;
            }

            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                manifest.retry_exhausted = true;
                publish_replay_manifest(root, manifest_path, manifest)?;
                break;
            };
            tokio::time::sleep(policy.retry_interval.min(remaining)).await;
        }
    }
    Ok(())
}

fn successful_receipt(
    generation_id: &str,
    frame: &ParsedFrame,
    outcome: ReplayOutcome,
    attempts: u64,
    durability: &Value,
) -> Result<ReplayReceipt> {
    let backend = durability
        .get("backend")
        .and_then(Value::as_str)
        .unwrap_or("database");
    let outcome_name = match &outcome {
        ReplayOutcome::Inserted => "inserted",
        ReplayOutcome::DuplicateIdentical => "duplicate_identical",
        _ => unreachable!("successful receipt requires an acknowledging outcome"),
    };
    Ok(ReplayReceipt {
        generation_id: generation_id.to_string(),
        offset: frame.offset,
        length: frame.length,
        record_digest: frame.digest.clone(),
        event_id: frame.event.id.clone(),
        content_digest: event_content_digest(&frame.event)?,
        outcome,
        attempts,
        error: None,
        database_evidence_ref: Some(format!("{backend}:{outcome_name}:sha256:{}", frame.digest)),
        database_evidence_at: Some(Utc::now().to_rfc3339()),
        conflict_snapshot_ref: None,
        conflict_snapshot_digest: None,
        resolution_ref: None,
        resolution_digest: None,
        resolved_authoritative: None,
        updated_at: Utc::now().to_rfc3339(),
    })
}

fn unresolved_receipt(
    generation_id: &str,
    frame: &ParsedFrame,
    outcome: ReplayOutcome,
    attempts: u64,
    error: String,
) -> Result<ReplayReceipt> {
    Ok(ReplayReceipt {
        generation_id: generation_id.to_string(),
        offset: frame.offset,
        length: frame.length,
        record_digest: frame.digest.clone(),
        event_id: frame.event.id.clone(),
        content_digest: event_content_digest(&frame.event)?,
        outcome,
        attempts,
        error: Some(error),
        database_evidence_ref: None,
        database_evidence_at: None,
        conflict_snapshot_ref: None,
        conflict_snapshot_digest: None,
        resolution_ref: None,
        resolution_digest: None,
        resolved_authoritative: None,
        updated_at: Utc::now().to_rfc3339(),
    })
}

#[allow(clippy::too_many_arguments)]
fn conflict_receipt(
    root: &Path,
    installation: &Installation,
    manifest: &ReplayManifest,
    frame: &ParsedFrame,
    attempts: u64,
    existing: crate::storage::BackgroundEventRow,
    verification_id: &str,
    backend: &str,
    transaction_isolation: &str,
) -> Result<ReplayReceipt> {
    let generation_id = &manifest.generation_id;
    let path = conflict_snapshot_path(
        root,
        generation_id,
        frame.offset,
        frame.length,
        &frame.digest,
    );
    let existing = ConflictRowEvidence {
        id: existing.id,
        kind: existing.kind,
        payload: existing.payload,
        created_at: existing.created_at,
    };
    let existing_row_digest = digest(&serde_json::to_vec(&existing)?);
    let snapshot = RecordedConflictSnapshot {
        format_version: FORMAT_VERSION,
        installation_id: installation.installation_id.clone(),
        generation_id: generation_id.clone(),
        journal_offset: frame.offset,
        journal_length: frame.length,
        journal_record_digest: frame.digest.clone(),
        source: ConflictRowEvidence {
            id: frame.event.id.clone(),
            kind: frame.event.kind.clone(),
            payload: frame.event.payload.to_string(),
            created_at: frame.event.created_at.clone(),
        },
        existing,
        existing_row_digest,
        verification_id: verification_id.to_string(),
        backend: backend.to_string(),
        transaction_isolation: transaction_isolation.to_string(),
        read_evidence: format!("{backend}:{transaction_isolation}:{verification_id}"),
        observed_at: Utc::now().to_rfc3339(),
    };
    let coordination_lock = open_coordination_lock(root)?;
    lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    verify_replay_publication_snapshot(root, manifest)?;
    if path.exists() {
        let recorded: Value = read_checked(&path)?;
        let expected = serde_json::to_value(&snapshot)?;
        for field in [
            "format_version",
            "installation_id",
            "generation_id",
            "journal_offset",
            "journal_length",
            "journal_record_digest",
            "source",
            "existing",
            "existing_row_digest",
            "verification_id",
            "backend",
            "transaction_isolation",
            "read_evidence",
        ] {
            if recorded.get(field) != expected.get(field) {
                bail!("protected replay conflict evidence changed");
            }
        }
    } else {
        write_checked(&path, &snapshot)?;
    }
    File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
    let snapshot_digest = digest(&fs::read(&path)?);
    Ok(ReplayReceipt {
        generation_id: generation_id.to_string(),
        offset: frame.offset,
        length: frame.length,
        record_digest: frame.digest.clone(),
        event_id: frame.event.id.clone(),
        content_digest: event_content_digest(&frame.event)?,
        outcome: ReplayOutcome::DuplicateConflict,
        attempts,
        error: None,
        database_evidence_ref: Some(verification_id.to_string()),
        database_evidence_at: Some(snapshot.observed_at.clone()),
        conflict_snapshot_ref: Some(path.display().to_string()),
        conflict_snapshot_digest: Some(snapshot_digest),
        resolution_ref: None,
        resolution_digest: None,
        resolved_authoritative: None,
        updated_at: Utc::now().to_rfc3339(),
    })
}

fn persist_replay_manifest(path: &Path, manifest: &mut ReplayManifest) -> Result<()> {
    refresh_manifest_summary(manifest)?;
    manifest.revision += 1;
    manifest.updated_at = Utc::now().to_rfc3339();
    write_checked(path, manifest)
}

fn publish_replay_manifest(root: &Path, path: &Path, manifest: &mut ReplayManifest) -> Result<()> {
    let coordination_lock = open_coordination_lock(root)?;
    lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    verify_replay_publication_snapshot(root, manifest)?;
    persist_replay_manifest(path, manifest)?;
    File::unlock(&coordination_lock).context("unlocking accounting coordination")
}

fn verify_replay_publication_snapshot(root: &Path, manifest: &ReplayManifest) -> Result<()> {
    let coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    let state: GenerationState =
        read_checked(&generation_state_path(root, &manifest.generation_id))?;
    let journal = fs::read(generation_journal_path(root, &manifest.generation_id))?;
    if coordination.revision != manifest.source_coordination_revision
        || state.revision != manifest.source_generation_revision
        || !matches!(
            state.phase,
            GenerationPhase::Active | GenerationPhase::Draining | GenerationPhase::Adopting
        )
        || coordination
            .coverage
            .iter()
            .find(|entry| entry.generation_id == manifest.generation_id)
            != Some(&coverage(&state))
        || journal.len() as u64 != manifest.journal_length
        || digest(&journal) != manifest.journal_evidence_digest
    {
        bail!("accounting generation changed before replay evidence publication");
    }
    Ok(())
}

fn parser_result(parsed: &ParsedJournal) -> &'static str {
    if parsed.torn_tail.is_some() {
        "torn_tail"
    } else if !parsed.malformed.is_empty() {
        "malformed_frame"
    } else {
        "complete"
    }
}

fn replay_outcome_name(outcome: &ReplayOutcome) -> &'static str {
    match outcome {
        ReplayOutcome::Inserted => "inserted",
        ReplayOutcome::DuplicateIdentical => "duplicate_identical",
        ReplayOutcome::DuplicateConflict => "duplicate_conflict",
        ReplayOutcome::Failed => "failed",
        ReplayOutcome::Unknown => "unknown",
    }
}

fn receipt_summary(receipts: &[ReplayReceipt]) -> Result<(String, BTreeMap<String, u64>)> {
    let mut counts = [
        "inserted",
        "duplicate_identical",
        "duplicate_conflict",
        "failed",
        "unknown",
    ]
    .into_iter()
    .map(|outcome| (outcome.to_string(), 0))
    .collect::<BTreeMap<_, _>>();
    for receipt in receipts {
        *counts
            .get_mut(replay_outcome_name(&receipt.outcome))
            .expect("all replay outcomes are initialized") += 1;
    }
    Ok((digest(&serde_json::to_vec(receipts)?), counts))
}

fn refresh_manifest_summary(manifest: &mut ReplayManifest) -> Result<()> {
    manifest.receipts.sort_by_key(|receipt| receipt.offset);
    let (receipt_digest, outcome_counts) = receipt_summary(&manifest.receipts)?;
    manifest.ordered_receipt_digest = receipt_digest;
    manifest.outcome_counts = outcome_counts;
    Ok(())
}

fn validate_manifest_summary(manifest: &ReplayManifest) -> Result<()> {
    let (receipt_digest, outcome_counts) = receipt_summary(&manifest.receipts)?;
    if manifest.ordered_receipt_digest != receipt_digest
        || manifest.outcome_counts != outcome_counts
        || !matches!(
            manifest.parser_result.as_str(),
            "complete" | "malformed_frame" | "torn_tail"
        )
        || manifest.sync_result != "synced"
        || manifest
            .receipts
            .windows(2)
            .any(|pair| pair[0].offset >= pair[1].offset)
    {
        bail!("replay manifest summary evidence is inconsistent");
    }
    Ok(())
}

fn event_content_digest(event: &AccountingEvent) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&serde_json::json!({
        "id": event.id,
        "kind": event.kind,
        "payload": event.payload.to_string(),
        "created_at": event.created_at,
    }))?))
}

fn verify_adoption_copy(adoption: &Adoption, require_source: bool) -> Result<()> {
    let backup = fs::read(&adoption.backup).context("reading retained legacy backup")?;
    if backup.len() as u64 != adoption.source_length || digest(&backup) != adoption.source_digest {
        bail!("retained legacy backup does not match adoption evidence");
    }
    let source = Path::new(&adoption.source);
    if require_source || source.exists() {
        let source_bytes = fs::read(source).context("reading retained legacy source")?;
        if source_bytes.len() as u64 != adoption.source_length
            || digest(&source_bytes) != adoption.source_digest
        {
            bail!("retained legacy source does not match adoption evidence");
        }
    }
    Ok(())
}

fn validate_complete_adoption(
    root: &Path,
    installation: &Installation,
    adoption: &Adoption,
) -> Result<()> {
    let source = Path::new(&adoption.source);
    let expected_backup = source.with_file_name(format!(
        "{}.steve-backup",
        source
            .file_name()
            .and_then(|name| name.to_str())
            .context("completed adoption source name is invalid")?
    ));
    let expected_retained = retained_source_path(source)?;
    let expected_outcome = if adoption.source_length == 0 {
        "empty_vacuous"
    } else {
        "reconciled"
    };
    if adoption.format_version != FORMAT_VERSION
        || adoption.installation_id != installation.installation_id
        || adoption.state != "complete"
        || !adoption.source_directory_synced
        || adoption.record_outcome != expected_outcome
        || Path::new(&adoption.backup) != expected_backup
        || adoption.retained_source.as_deref() != expected_retained.to_str()
    {
        bail!("completed adoption metadata is inconsistent");
    }
    let backup =
        fs::read(&adoption.backup).context("completed adoption backup does not match evidence")?;
    if backup.len() as u64 != adoption.source_length || digest(&backup) != adoption.source_digest {
        bail!("completed adoption backup does not match evidence");
    }
    if source.exists() {
        bail!("completed adoption original source still exists");
    }
    let retained = adoption
        .retained_source
        .as_deref()
        .map(Path::new)
        .context("completed adoption retained source does not match evidence")?;
    let retained_bytes =
        fs::read(retained).context("completed adoption retained source does not match evidence")?;
    if retained_bytes.len() as u64 != adoption.source_length
        || digest(&retained_bytes) != adoption.source_digest
    {
        bail!("completed adoption retained source does not match evidence");
    }

    let state: GenerationState =
        read_checked(&generation_state_path(root, &adoption.generation_id))?;
    let manifest_path = replay_manifest_path(root, &adoption.generation_id);
    let manifest_digest = digest(&fs::read(&manifest_path)?);
    let expected_manifest = manifest_path.display().to_string();
    let expected_evidence_ref = format!("sha256:{manifest_digest}");
    if state.phase != GenerationPhase::Reconciled
        || state.format_version != FORMAT_VERSION
        || state.installation_id != installation.installation_id
        || state.generation_id != adoption.generation_id
        || state.replay_manifest.as_deref() != Some(expected_manifest.as_str())
        || state.replay_manifest_digest.as_deref() != Some(manifest_digest.as_str())
        || adoption.accepted_evidence_ref.as_deref() != Some(expected_evidence_ref.as_str())
    {
        bail!("completed adoption replay evidence is inconsistent");
    }
    let manifest: ReplayManifest = read_checked(&manifest_path)?;
    validate_manifest_summary(&manifest)?;
    let database_committed = manifest.outcome_counts["inserted"]
        + manifest.outcome_counts["duplicate_identical"]
        + manifest
            .receipts
            .iter()
            .filter(|receipt| {
                receipt.outcome == ReplayOutcome::DuplicateConflict
                    && receipt.resolution_ref.is_some()
            })
            .count() as u64;
    if manifest.format_version != FORMAT_VERSION
        || manifest.installation_id != installation.installation_id
        || manifest.generation_id != adoption.generation_id
        || manifest.journal_length != adoption.source_length
        || manifest.journal_evidence_digest != adoption.source_digest
        || state.admitted_count != Some(0)
        || state.worker_completed_count != Some(0)
        || state.journal_synced_count != Some(manifest.complete_record_count)
        || state.database_committed_count != Some(database_committed)
    {
        bail!("completed adoption replay evidence is inconsistent");
    }
    Ok(())
}

fn finish_reconciled_adoption(
    root: &Path,
    adoption: &mut Adoption,
    state: &GenerationState,
    manifest_path: &Path,
) -> Result<()> {
    let manifest_digest = digest(&fs::read(manifest_path)?);
    let expected_manifest = manifest_path.display().to_string();
    if state.phase != GenerationPhase::Reconciled
        || state.replay_manifest.as_deref() != Some(expected_manifest.as_str())
        || state.replay_manifest_digest.as_deref() != Some(manifest_digest.as_str())
    {
        bail!("reconciled adoption lacks matching replay evidence");
    }
    verify_adoption_copy(adoption, false)?;
    let source = PathBuf::from(&adoption.source);
    let retained = match adoption.retained_source.as_deref() {
        Some(path) => PathBuf::from(path),
        None => retained_source_path(&source)?,
    };
    if adoption.state != "retaining_source" {
        if !source.exists() || retained.exists() {
            bail!("reconciled legacy source move is ambiguous");
        }
        verify_adoption_copy(adoption, true)?;
        adoption.state = "retaining_source".into();
        adoption.record_outcome = "reconciled".into();
        adoption.accepted_evidence_ref = state
            .replay_manifest_digest
            .as_ref()
            .map(|digest| format!("sha256:{digest}"));
        adoption.retained_source = Some(retained.display().to_string());
        adoption.source_directory_synced = false;
        adoption.updated_at = Utc::now().to_rfc3339();
        write_checked(&root.join("adoption.json"), adoption)?;
    }

    if source.exists() {
        verify_adoption_copy(adoption, true)?;
        if retained.exists() {
            bail!("reconciled legacy source move is ambiguous");
        }
        fs::rename(&source, &retained).context("retaining reconciled legacy source")?;
    } else {
        let retained_bytes = fs::read(&retained).context("reading retained legacy source")?;
        if retained_bytes.len() as u64 != adoption.source_length
            || digest(&retained_bytes) != adoption.source_digest
        {
            bail!("retained legacy source does not match adoption evidence");
        }
    }
    sync_directory(source.parent().context("legacy source has no parent")?)?;
    adoption.state = "complete".into();
    adoption.source_directory_synced = true;
    adoption.updated_at = Utc::now().to_rfc3339();
    write_checked(&root.join("adoption.json"), adoption)
}

fn replay_manifest_path(root: &Path, generation_id: &str) -> PathBuf {
    root.join("generations")
        .join(format!("{generation_id}.replay.json"))
}

fn conflict_snapshot_path(
    root: &Path,
    generation_id: &str,
    frame_offset: u64,
    frame_length: u64,
    frame_digest: &str,
) -> PathBuf {
    root.join("generations").join(format!(
        "{generation_id}.conflict.{frame_offset}.{frame_length}.{frame_digest}.json"
    ))
}

fn recover_reconciled_coverage_gap(
    root: &Path,
    coordination: &mut Coordination,
    state: &GenerationState,
) -> Result<()> {
    if state.phase != GenerationPhase::Reconciled
        || coordination.coverage.contains(&coverage(state))
    {
        return Ok(());
    }
    let prior = coordination
        .coverage
        .iter()
        .find(|entry| entry.generation_id == state.generation_id)
        .context("reconciled generation is missing prior coverage")?;
    let manifest_path = state
        .replay_manifest
        .as_deref()
        .map(Path::new)
        .context("reconciled generation is missing replay manifest")?;
    let manifest: ReplayManifest = read_checked(manifest_path)?;
    let manifest_digest = digest(&fs::read(manifest_path)?);
    for receipt in &manifest.receipts {
        if receipt.outcome.acknowledges() {
            continue;
        }
        if receipt.outcome != ReplayOutcome::DuplicateConflict {
            bail!("reconciled generation retains an unresolved replay receipt");
        }
        verify_conflict_resolution_evidence(root, receipt)
            .context("reconciled conflict receipt lacks verified resolution evidence")?;
    }
    if prior.generation_state_revision + 1 != state.revision {
        bail!("reconciled state revision does not continue prior coverage");
    }
    if prior.journal_evidence_digest != state.journal_evidence_digest
        || manifest.journal_length != state.journal_length
        || manifest.journal_evidence_digest != state.journal_evidence_digest
        || digest(&fs::read(&state.journal)?) != state.journal_evidence_digest
    {
        bail!("reconciled journal does not match prior coverage and manifest evidence");
    }
    if manifest_path != replay_manifest_path(root, &state.generation_id) {
        bail!("reconciled manifest path is not canonical");
    }
    if state.replay_manifest_digest.as_deref() != Some(manifest_digest.as_str()) {
        bail!("reconciled state does not bind the final manifest digest");
    }
    if manifest.source_coordination_revision > coordination.revision {
        bail!("reconciled manifest refers to a future coordination revision");
    }
    if manifest.source_generation_revision != prior.generation_state_revision {
        bail!("reconciled manifest does not bind prior generation revision");
    }
    if state.last_complete_record_boundary != Some(state.journal_length)
        || manifest.complete_boundary != state.journal_length
        || manifest.receipts.len() as u64 != manifest.complete_record_count
        || !manifest.malformed_frames.is_empty()
        || manifest.torn_tail_length != 0
    {
        bail!("reconciled manifest does not prove a complete replay boundary");
    }
    coordination.revision += 1;
    replace_coverage(&mut coordination.coverage, coverage(state));
    write_checked(&root.join("coordination.json"), coordination)
}

fn recover_reconciled_coverage_gaps(root: &Path, coordination: &mut Coordination) -> Result<()> {
    for entry in coordination.coverage.clone() {
        let state: GenerationState =
            read_checked(&generation_state_path(root, &entry.generation_id))?;
        recover_reconciled_coverage_gap(root, coordination, &state)?;
    }
    Ok(())
}

fn recover_unresolved_coverage_gaps(root: &Path, coordination: &mut Coordination) -> Result<()> {
    let mut changed = false;
    for prior in coordination.coverage.clone() {
        let state: GenerationState =
            read_checked(&generation_state_path(root, &prior.generation_id))?;
        if coverage(&state) == prior {
            continue;
        }
        if !matches!(
            state.phase,
            GenerationPhase::Active | GenerationPhase::Draining
        ) || state.revision <= prior.generation_state_revision
        {
            bail!("unresolved generation coverage gap is not recoverable");
        }
        let journal = fs::read(generation_journal_path(root, &state.generation_id))?;
        if journal.len() as u64 != state.journal_length
            || digest(&journal) != state.journal_evidence_digest
        {
            bail!("unresolved generation journal does not match checked state");
        }
        replace_coverage(&mut coordination.coverage, coverage(&state));
        changed = true;
    }
    if changed {
        coordination.revision = coordination.revision.saturating_add(1);
        write_checked(&root.join("coordination.json"), coordination)?;
    }
    Ok(())
}

fn resume_empty_adoption_after_move(
    source: &Path,
    root: &Path,
    maintenance_assertion: &Path,
) -> Result<String> {
    let root = resolve_accounting_root(root)?;
    let coordination_lock = open_coordination_lock(&root)?;
    lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
    recover_checked_publications(&root)?;
    let installation = load_provisioned_root_locked(&root)?;
    let adoption_path = root.join("adoption.json");
    let mut adoption: Adoption = read_checked(&adoption_path)?;
    let assertion = read_maintenance_assertion(maintenance_assertion, source)?;
    let manifest_path = replay_manifest_path(&root, &adoption.generation_id);
    let accepted_evidence_ref = format!("sha256:{}", digest(&fs::read(&manifest_path)?));
    if adoption.installation_id != installation.installation_id
        || !matches!(adoption.state.as_str(), "retaining_source" | "complete")
        || adoption.source != source.display().to_string()
        || adoption.source_length != 0
        || adoption.source_digest != digest(&[])
        || adoption.maintenance_assertion != assertion
        || adoption.record_outcome != "empty_vacuous"
        || adoption.accepted_evidence_ref != Some(accepted_evidence_ref)
    {
        bail!("interrupted empty legacy adoption evidence is inconsistent");
    }
    let retained = adoption
        .retained_source
        .as_deref()
        .map(Path::new)
        .context("interrupted empty adoption is missing retained source")?;
    if source.exists()
        || !retained.is_file()
        || !fs::read(retained)?.is_empty()
        || !fs::read(&adoption.backup)?.is_empty()
    {
        bail!("interrupted empty legacy source move is ambiguous");
    }
    let coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    let states = verify_generations(&root, &installation, &coordination)?;
    if states
        .get(&adoption.generation_id)
        .map(|state| &state.phase)
        != Some(&GenerationPhase::Reconciled)
    {
        bail!("empty legacy generation is not reconciled");
    }
    if adoption.state == "complete" {
        if !adoption.source_directory_synced {
            bail!("completed empty adoption lacks source directory sync evidence");
        }
        validate_complete_adoption(&root, &installation, &adoption)?;
        File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
        return Ok(format!(
            "empty legacy accounting journal already adopted for generation {}",
            adoption.generation_id
        ));
    }
    sync_directory(source.parent().context("legacy source has no parent")?)?;
    adoption.state = "complete".into();
    adoption.source_directory_synced = true;
    adoption.updated_at = Utc::now().to_rfc3339();
    write_checked(&adoption_path, &adoption)?;
    File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
    Ok(format!(
        "empty legacy accounting journal adoption resumed for generation {}",
        adoption.generation_id
    ))
}

fn retained_source_path(source: &Path) -> Result<PathBuf> {
    Ok(source.with_file_name(format!(
        "{}.steve-retained",
        source
            .file_name()
            .and_then(|name| name.to_str())
            .context("legacy journal name is not valid UTF-8")?
    )))
}

fn canonicalize_missing_path(path: &Path) -> Result<PathBuf> {
    let parent = fs::canonicalize(path.parent().context("path has no parent")?)
        .with_context(|| format!("resolving parent for missing path {}", path.display()))?;
    Ok(parent.join(path.file_name().context("path has no filename")?))
}

fn start_writer(
    root: PathBuf,
    mut journal: File,
    receiver: mpsc::Receiver<JournalMessage>,
    state: Arc<Mutex<GenerationState>>,
    incident: Arc<Mutex<Incident>>,
    incident_publisher: mpsc::Sender<IncidentPublisherMessage>,
    incident_submitted: Arc<AtomicU64>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("steve-accounting-journal".into())
        .spawn(move || {
            let mut hasher = Sha256::new();
            while let Ok(message) = receiver.recv() {
                let event = match message {
                    JournalMessage::Event(event) => event,
                    JournalMessage::Shutdown(completed) => {
                        if let Err(err) = journal.sync_all() {
                            tracing::error!(%err, "accounting journal final sync failed");
                            if let Err(latch_error) = queue_incident_publication(
                                &incident,
                                &incident_publisher,
                                &incident_submitted,
                                IncidentCause::JournalWriteFailed,
                                0,
                                1,
                            ) {
                                tracing::error!(%latch_error, "accounting incident publication failed");
                            }
                        }
                        let _ = completed.send(());
                        return;
                    }
                };
                if let Err(err) = append_event(&root, &mut journal, &mut hasher, &state, &event) {
                    tracing::error!(%err, event_id = %event.id, "accounting journal write failed");
                    if let Err(latch_error) = queue_incident_publication(
                        &incident,
                        &incident_publisher,
                        &incident_submitted,
                        IncidentCause::JournalWriteFailed,
                        0,
                        1,
                    ) {
                        tracing::error!(%latch_error, "accounting incident publication failed");
                    }
                }
            }
        })
        .context("spawning accounting journal writer")
}

fn append_event(
    root: &Path,
    journal: &mut File,
    hasher: &mut Sha256,
    state: &Mutex<GenerationState>,
    event: &AccountingEvent,
) -> Result<()> {
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    let lock = open_coordination_lock(root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    recover_checked_publications(root)?;
    let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    journal.write_all(&line)?;
    journal.sync_all()?;
    hasher.update(&line);
    let mut state = state.lock().expect("accounting generation state");
    state.revision += 1;
    state.journal_length += line.len() as u64;
    state.journal_evidence_digest = format_digest(hasher.clone().finalize());
    state.updated_at = Utc::now().to_rfc3339();
    write_checked(&generation_state_path(root, &state.generation_id), &*state)?;
    coordination.revision += 1;
    replace_coverage(&mut coordination.coverage, coverage(&state));
    write_checked(&root.join("coordination.json"), &coordination)?;
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(())
}

fn load_snapshot(root: &Path) -> Result<AccountingSnapshot> {
    let root = resolve_accounting_root(root)?;
    let lock = open_coordination_lock(&root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    let started = Instant::now();
    recover_checked_publications(&root)?;
    let installation = load_provisioned_root_locked(&root)?;
    let coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    verify_generations(&root, &installation, &coordination)?;
    ensure_fresh(started)?;
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(AccountingSnapshot {
        installation_id: coordination.installation_id,
        revision: coordination.revision,
        coverage: coordination.coverage,
    })
}

fn resolve_accounting_root(root: &Path) -> Result<PathBuf> {
    require_absolute(root, "accounting root")?;
    if !root.exists() {
        bail!("accounting root {} is not provisioned", root.display());
    }
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving accounting root {}", root.display()))?;
    if !root.is_dir() {
        bail!("accounting root {} is not a directory", root.display());
    }
    ensure_private_directory(&root)?;
    Ok(root)
}

fn load_provisioned_root_locked(root: &Path) -> Result<Installation> {
    verify_root_inventory(root)?;
    let installation: Installation = read_checked(&root.join("installation.json"))?;
    let incident: Incident = read_checked(&root.join("incident.json"))?;
    let audit: Vec<AuditRecord> = read_checked(&root.join("audit.json"))?;
    let provisioning: Provisioning = read_checked(&root.join("provisioning.json"))?;
    let coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    if installation.format_version != FORMAT_VERSION
        || incident.format_version != FORMAT_VERSION
        || provisioning.format_version != FORMAT_VERSION
        || coordination.format_version != FORMAT_VERSION
    {
        bail!("unsupported accounting ownership evidence format");
    }
    if installation.root != root.display().to_string() {
        bail!("accounting root moved since provisioning");
    }
    if provisioning.root != installation.root
        || provisioning.inventory != Vec::<String>::new()
        || !provisioning.files_synced
        || !provisioning.directory_synced
    {
        bail!("accounting provisioning evidence does not match the private root");
    }
    if incident.installation_id != installation.installation_id
        || provisioning.installation_id != installation.installation_id
        || coordination.installation_id != installation.installation_id
    {
        bail!("accounting ownership evidence has mismatched installation identities");
    }
    for record in &audit {
        validate_audit_record(record, &installation.installation_id)?;
    }
    if !root.join("coordination.lock").is_file()
        || !root.join("maintenance.lock").is_file()
        || !root.join("generations").is_dir()
    {
        bail!("accounting ownership evidence is incomplete");
    }
    validate_incident(&incident)?;
    Ok(installation)
}

fn validate_incident(incident: &Incident) -> Result<()> {
    match incident.state {
        IncidentState::Clear => {
            if incident.incident_id.is_some()
                || incident.first_observed_at.is_some()
                || incident.cause.is_some()
                || incident.disposition.is_some()
                || incident.payloads.pending_replay.volatile != Some(0)
                || incident.payloads.pending_replay.durable != Some(0)
                || incident.payloads.provisional.unknown != Some(0)
                || incident.payloads.outcome_totals.reconciled != Some(0)
                || incident.payloads.outcome_totals.unrecoverable_lost != Some(0)
            {
                bail!("clear accounting incident evidence is inconsistent");
            }
        }
        IncidentState::Blocked => {
            if incident.incident_id.as_deref().is_none_or(str::is_empty)
                || incident
                    .first_observed_at
                    .as_deref()
                    .is_none_or(str::is_empty)
                || incident.cause.is_none()
                || incident.disposition.is_some()
            {
                bail!("blocked accounting incident evidence is incomplete");
            }
        }
        IncidentState::Unreconciled => {
            if incident.cause.is_none() || incident.disposition.is_some() {
                bail!("unreconciled accounting incident evidence is inconsistent");
            }
        }
        IncidentState::Acknowledged => {
            let disposition = incident
                .disposition
                .as_ref()
                .context("acknowledged incident lacks disposition")?;
            if incident.incident_id.as_deref().is_none_or(str::is_empty)
                || incident
                    .first_observed_at
                    .as_deref()
                    .is_none_or(str::is_empty)
                || incident.cause.is_none()
                || incident.payloads.pending_replay.volatile != Some(0)
                || incident.payloads.pending_replay.durable != Some(0)
                || !disposition_matches_incident(&disposition.kind, incident)
            {
                bail!("acknowledged accounting incident evidence is inconsistent");
            }
            validate_disposition(disposition)?;
        }
    }
    if let Some(observed) = incident.first_observed_at.as_deref() {
        chrono::DateTime::parse_from_rfc3339(observed)
            .context("accounting incident first_observed_at is not RFC3339")?;
    }
    Ok(())
}

fn validate_disposition(disposition: &Disposition) -> Result<()> {
    if !matches!(
        disposition.kind.as_str(),
        "accepted_loss" | "accepted_uncertainty" | "accepted_loss_and_uncertainty"
    ) || disposition.actor.trim().is_empty()
        || disposition.evidence_ref.trim().is_empty()
    {
        bail!("accounting disposition identity or evidence reference is empty");
    }
    chrono::DateTime::parse_from_rfc3339(&disposition.at)
        .context("accounting disposition time is not RFC3339")?;
    Ok(())
}

fn validate_audit_record(record: &AuditRecord, installation_id: &str) -> Result<()> {
    if record.format_version != FORMAT_VERSION || record.installation_id != installation_id {
        bail!("unsupported or mismatched accounting audit evidence");
    }
    if record.incident_id.trim().is_empty()
        || record.evidence_ref.trim().is_empty()
        || record.actor.trim().is_empty()
        || record.revision != record.previous_revision.saturating_add(1)
    {
        bail!("accounting audit evidence is incomplete");
    }
    chrono::DateTime::parse_from_rfc3339(&record.at)
        .context("accounting audit time is not RFC3339")?;
    let mut coverage = record.coverage.clone();
    sort_coverage(&mut coverage);
    if coverage != record.coverage
        || coverage
            .windows(2)
            .any(|pair| pair[0].generation_id == pair[1].generation_id)
    {
        bail!("accounting audit coverage is not canonical");
    }
    match record.operation.as_str() {
        "acknowledge" => {
            let disposition = record
                .disposition
                .as_ref()
                .context("acknowledgement audit lacks disposition")?;
            validate_disposition(disposition)?;
            if record.event_id.is_some()
                || record.authoritative.is_some()
                || record.conflict_snapshot_ref.is_some()
                || record.conflict_snapshot_digest.is_some()
                || record.final_row.is_some()
            {
                bail!("acknowledgement audit contains conflict fields");
            }
        }
        "prepare_conflict_resolution" | "resolve_conflict" => {
            if record.disposition.is_some()
                || record.event_id.as_deref().is_none_or(str::is_empty)
                || !matches!(
                    record.authoritative.as_deref(),
                    Some("journal" | "database")
                )
                || record
                    .conflict_snapshot_ref
                    .as_deref()
                    .is_none_or(str::is_empty)
                || record
                    .conflict_snapshot_digest
                    .as_deref()
                    .is_none_or(str::is_empty)
                || record.final_row.is_none()
            {
                bail!("conflict resolution audit is incomplete");
            }
        }
        _ => bail!("unsupported accounting audit operation"),
    }
    Ok(())
}

fn validate_acknowledgement_binding(
    root: &Path,
    record: &AuditRecord,
    incident: &Incident,
    states: &BTreeMap<String, GenerationState>,
) -> Result<()> {
    if record.operation != "acknowledge"
        || record.incident_id != incident.incident_id.as_deref().unwrap_or_default()
        || record.revision != incident.revision
        || record.disposition != incident.disposition
        || incident.payloads.pending_replay.volatile != Some(0)
        || incident.payloads.pending_replay.durable != Some(0)
    {
        bail!("acknowledged incident does not match its audit evidence");
    }
    let disposition = incident
        .disposition
        .as_ref()
        .context("acknowledged incident lacks disposition")?;
    if record.actor != disposition.actor
        || record.at != disposition.at
        || record.evidence_ref != disposition.evidence_ref
    {
        bail!("acknowledgement audit identity does not match its disposition");
    }
    for state in states.values() {
        if record
            .coverage
            .iter()
            .any(|accepted| accepted.generation_id == state.generation_id)
        {
            continue;
        }
        if state.phase != GenerationPhase::Active {
            bail!("acknowledgement audit omits non-live generation coverage");
        }
        let journal = OpenOptions::new()
            .read(true)
            .write(true)
            .open(generation_journal_path(root, &state.generation_id))?;
        match journal.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => {}
            Ok(()) => {
                File::unlock(&journal)?;
                bail!("acknowledgement audit omits unlocked generation coverage");
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).context("checking omitted acknowledgement generation")
            }
        }
    }
    for accepted in &record.coverage {
        let current = states
            .get(&accepted.generation_id)
            .context("acknowledgement coverage generation is missing")?;
        if coverage(current) == *accepted {
            continue;
        }
        let manifest_path = current
            .replay_manifest
            .as_deref()
            .map(Path::new)
            .context("acknowledgement coverage changed without replay evidence")?;
        let manifest: ReplayManifest = read_checked(manifest_path)?;
        if current.phase != GenerationPhase::Reconciled
            || current.revision != accepted.generation_state_revision.saturating_add(1)
            || current.journal_evidence_digest != accepted.journal_evidence_digest
            || manifest.source_generation_revision != accepted.generation_state_revision
            || manifest.journal_evidence_digest != accepted.journal_evidence_digest
            || manifest.parser_result != "complete"
            || manifest.retry_exhausted
        {
            bail!("acknowledgement coverage no longer matches generation evidence");
        }
    }
    Ok(())
}

fn ensure_fresh(started: Instant) -> Result<()> {
    ensure_fresh_with_bound(started, SNAPSHOT_FRESHNESS)
}

fn ensure_fresh_with_bound(started: Instant, bound: Duration) -> Result<()> {
    if started.elapsed() > bound {
        bail!("accounting ownership snapshot expired before completion");
    }
    Ok(())
}

fn verify_generations(
    root: &Path,
    installation: &Installation,
    coordination: &Coordination,
) -> Result<BTreeMap<String, GenerationState>> {
    verify_generations_with_adoption(root, installation, coordination, false)
}

fn verify_generations_with_adoption(
    root: &Path,
    installation: &Installation,
    coordination: &Coordination,
    allow_unlocked_replayable: bool,
) -> Result<BTreeMap<String, GenerationState>> {
    let generation_root = root.join("generations");
    let mut states = BTreeMap::new();
    let mut state_checksums = BTreeSet::new();
    let mut journals = BTreeSet::new();
    let mut replay_manifests = BTreeSet::new();
    let mut replay_checksums = BTreeSet::new();
    let mut conflict_snapshots = BTreeSet::new();
    let mut conflict_checksums = BTreeSet::new();
    for entry in fs::read_dir(&generation_root)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("generation filename is not valid UTF-8")?;
        if let Some(id) = name.strip_suffix(".state.json.sha256") {
            state_checksums.insert(id.to_string());
        } else if let Some(id) = name.strip_suffix(".replay.json.sha256") {
            replay_checksums.insert(id.to_string());
        } else if let Some(id) = name.strip_suffix(".replay.json") {
            replay_manifests.insert(id.to_string());
        } else if let Some(target) = name.strip_suffix(".sha256") {
            if is_conflict_snapshot_name(target) {
                conflict_checksums.insert(target.to_string());
            } else {
                bail!("unknown accounting generation artifact {}", path.display());
            }
        } else if is_conflict_snapshot_name(name) {
            conflict_snapshots.insert(name.to_string());
        } else if let Some(id) = name.strip_suffix(".state.json") {
            let state: GenerationState = read_checked(&path)?;
            if state.format_version != FORMAT_VERSION
                || state.installation_id != installation.installation_id
                || state.generation_id != id
            {
                bail!(
                    "generation state {} has mismatched ownership",
                    path.display()
                );
            }
            states.insert(id.to_string(), state);
        } else if let Some(id) = name.strip_suffix(".journal") {
            journals.insert(id.to_string());
        } else if name.ends_with(".staging") {
            bail!(
                "unresolved generation staging evidence at {}",
                path.display()
            );
        } else {
            bail!("unknown accounting generation artifact {}", path.display());
        }
    }

    if journals != states.keys().cloned().collect() {
        bail!("generation journal/state inventory is incomplete");
    }
    if state_checksums != states.keys().cloned().collect() {
        bail!("generation state/checksum inventory is incomplete");
    }
    if replay_manifests != replay_checksums {
        bail!("generation replay/checksum inventory is incomplete");
    }
    if conflict_snapshots != conflict_checksums {
        bail!("generation conflict/checksum inventory is incomplete");
    }
    if replay_manifests
        .iter()
        .any(|generation_id| !states.contains_key(generation_id))
    {
        bail!("generation replay evidence has no owned generation");
    }
    if conflict_snapshots.iter().any(|name| {
        conflict_snapshot_generation(name)
            .is_none_or(|generation_id| !states.contains_key(generation_id))
    }) {
        bail!("generation conflict evidence has no owned generation");
    }
    let mut expected = states.values().map(coverage).collect::<Vec<_>>();
    sort_coverage(&mut expected);
    let mut actual = coordination.coverage.clone();
    sort_coverage(&mut actual);
    if expected != actual {
        bail!("coordination coverage does not match generation evidence");
    }

    for state in states.values() {
        let path = generation_journal_path(root, &state.generation_id);
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        match file.try_lock() {
            Ok(()) => {
                File::unlock(&file)?;
                if matches!(
                    state.phase,
                    GenerationPhase::Active | GenerationPhase::Draining | GenerationPhase::Adopting
                ) && !allow_unlocked_replayable
                {
                    bail!(
                        "generation {} has unresolved {:?} evidence",
                        state.generation_id,
                        state.phase
                    );
                }
                let bytes = fs::read(&path)?;
                if bytes.len() as u64 != state.journal_length
                    || digest(&bytes) != state.journal_evidence_digest
                {
                    bail!("unlocked generation journal evidence changed");
                }
                if state.phase == GenerationPhase::Reconciled && state.journal_length > 0 {
                    let manifest_path = state
                        .replay_manifest
                        .as_deref()
                        .map(Path::new)
                        .context("reconciled generation is missing replay evidence")?;
                    let manifest_digest = digest(&fs::read(manifest_path)?);
                    if manifest_path != replay_manifest_path(root, &state.generation_id)
                        || state.replay_manifest_digest.as_deref() != Some(manifest_digest.as_str())
                    {
                        bail!("reconciled generation replay evidence changed");
                    }
                    let _: ReplayManifest = read_checked(manifest_path)?;
                }
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if state.phase != GenerationPhase::Active {
                    bail!("busy generation {} is not active", state.generation_id);
                }
            }
            Err(std::fs::TryLockError::Error(err)) => {
                return Err(err).context("checking generation journal ownership")
            }
        }
    }
    Ok(states)
}

fn cleanup_empty_staging(root: &Path) -> Result<()> {
    let generation_root = root.join("generations");
    for entry in fs::read_dir(&generation_root)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("generation staging filename is not valid UTF-8")?;
        if !name.ends_with(".staging") {
            continue;
        }
        let Some(generation_id) = name.strip_suffix(".journal.staging") else {
            bail!("unknown generation staging artifact {}", path.display());
        };
        if generation_journal_path(root, generation_id).exists()
            || generation_state_path(root, generation_id).exists()
        {
            bail!(
                "generation staging {} conflicts with published evidence",
                path.display()
            );
        }
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        match file.try_lock() {
            Ok(()) if file.metadata()?.len() == 0 => {
                fs::remove_file(&path)?;
                sync_directory(&generation_root)?;
            }
            Ok(()) => {
                File::unlock(&file)?;
                bail!("nonempty generation staging evidence at {}", path.display());
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                bail!("busy generation staging evidence at {}", path.display());
            }
            Err(std::fs::TryLockError::Error(err)) => {
                return Err(err).context("checking generation staging ownership")
            }
        }
    }
    Ok(())
}

fn ensure_adoption_matches(
    adoption: &Adoption,
    installation: &Installation,
    source: &Path,
    backup: &Path,
    bytes: &[u8],
    assertion: &MaintenanceAssertion,
) -> Result<()> {
    if adoption.format_version != FORMAT_VERSION
        || adoption.installation_id != installation.installation_id
        || adoption.source != source.display().to_string()
        || adoption.backup != backup.display().to_string()
        || adoption.source_length != bytes.len() as u64
        || adoption.source_digest != digest(bytes)
        || adoption.maintenance_assertion != *assertion
    {
        bail!("legacy adoption evidence does not match retained source");
    }
    let backup_bytes =
        fs::read(backup).with_context(|| format!("reading legacy backup {}", backup.display()))?;
    if backup_bytes != bytes {
        bail!("legacy backup no longer matches retained source");
    }
    Ok(())
}

fn read_maintenance_assertion(path: &Path, source: &Path) -> Result<MaintenanceAssertion> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("reading maintenance assertion {}", path.display()))?;
    if !path.is_file() {
        bail!(
            "maintenance assertion {} is not a regular file",
            path.display()
        );
    }
    let bytes = fs::read(&path)?;
    let input: MaintenanceAssertionInput = serde_json::from_slice(&bytes)
        .context("parsing operator-supplied maintenance assertion")?;
    if input.workload_identity.trim().is_empty()
        || input.host.trim().is_empty()
        || input.source.trim().is_empty()
        || input.observed_at.trim().is_empty()
        || input.command_or_exported_status.trim().is_empty()
        || !input.stopped
        || !input.restart_disabled
    {
        bail!("maintenance assertion is incomplete");
    }
    chrono::DateTime::parse_from_rfc3339(&input.observed_at)
        .context("maintenance assertion observed_at is not RFC3339")?;
    if input.source != source.display().to_string() {
        bail!("maintenance assertion source does not match legacy journal");
    }
    Ok(MaintenanceAssertion {
        assertion_kind: "operator_supplied_unauthenticated".into(),
        path: path.display().to_string(),
        digest: digest(&bytes),
        workload_identity: input.workload_identity,
        host: input.host,
        source: input.source,
        stopped: input.stopped,
        restart_disabled: input.restart_disabled,
        observed_at: input.observed_at,
        command_or_exported_status: input.command_or_exported_status,
    })
}

fn ensure_source_unchanged(source: &Path, expected: &[u8]) -> Result<()> {
    let current = fs::read(source)
        .with_context(|| format!("rechecking legacy journal {}", source.display()))?;
    if current.len() != expected.len() || digest(&current) != digest(expected) {
        bail!("legacy journal changed during adoption");
    }
    Ok(())
}

fn open_or_publish_import(
    root: &Path,
    journal_path: &Path,
    importing_path: &Path,
    bytes: &[u8],
) -> Result<File> {
    if journal_path.exists() {
        if importing_path.exists() {
            bail!("legacy import has both staging and published journals");
        }
        let journal = OpenOptions::new()
            .read(true)
            .write(true)
            .open(journal_path)?;
        lock_with_timeout(&journal, LOCK_TIMEOUT)?;
        if fs::read(journal_path)? != bytes {
            bail!("imported journal does not match retained legacy source");
        }
        sync_directory(&root.join("generations"))?;
        return Ok(journal);
    }

    let mut journal = match OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(importing_path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
            .read(true)
            .write(true)
            .open(importing_path)
            .context("opening interrupted legacy import")?,
        Err(err) => return Err(err).context("creating legacy import staging journal"),
    };
    lock_with_timeout(&journal, LOCK_TIMEOUT)?;
    journal.set_len(0)?;
    journal.seek(SeekFrom::Start(0))?;
    journal.write_all(bytes)?;
    journal.sync_all()?;
    fs::rename(importing_path, journal_path).with_context(|| {
        format!(
            "publishing imported journal {} as {}",
            importing_path.display(),
            journal_path.display()
        )
    })?;
    sync_directory(&root.join("generations"))?;
    Ok(journal)
}

fn verify_import_state(
    state: &GenerationState,
    installation: &Installation,
    adoption: &Adoption,
    journal_path: &Path,
    bytes: &[u8],
) -> Result<()> {
    if state.format_version != FORMAT_VERSION
        || state.installation_id != installation.installation_id
        || state.generation_id != adoption.generation_id
        || !(state.phase == GenerationPhase::Adopting
            || (bytes.is_empty() && state.phase == GenerationPhase::Reconciled))
        || state.journal != journal_path.display().to_string()
        || state.journal_length != bytes.len() as u64
        || state.journal_evidence_digest != digest(bytes)
    {
        bail!("imported generation evidence does not match retained source");
    }
    Ok(())
}

fn write_or_verify_backup(path: &Path, bytes: &[u8]) -> Result<()> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
            set_readonly_file(path)?;
            File::open(path)?.sync_all()?;
            sync_directory(path.parent().context("legacy backup has no parent")?)?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() || fs::read(path)? != bytes
            {
                bail!("existing legacy backup does not match source");
            }
            set_readonly_file(path)?;
            File::open(path)?.sync_all()?;
            sync_directory(path.parent().context("legacy backup has no parent")?)?;
        }
        Err(err) => return Err(err).context("creating durable legacy backup"),
    }
    Ok(())
}

fn verify_root_inventory(root: &Path) -> Result<()> {
    let mut allowed = BTreeSet::from([
        "audit.json".to_string(),
        "audit.json.sha256".to_string(),
        "coordination.lock".to_string(),
        "coordination.json".to_string(),
        "coordination.json.sha256".to_string(),
        "generations".to_string(),
        "incident.json".to_string(),
        "incident.json.sha256".to_string(),
        "installation.json".to_string(),
        "installation.json.sha256".to_string(),
        "maintenance.lock".to_string(),
        "provisioning.json".to_string(),
        "provisioning.json.sha256".to_string(),
    ]);
    if root.join("adoption.json").exists() {
        allowed.insert("adoption.json".into());
        allowed.insert("adoption.json.sha256".into());
    }
    for entry in fs::read_dir(root)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("accounting root contains a non-UTF-8 artifact"))?;
        if !allowed.contains(&name) {
            bail!("unknown accounting ownership artifact {name}");
        }
    }
    Ok(())
}

fn coverage(state: &GenerationState) -> Coverage {
    Coverage {
        generation_id: state.generation_id.clone(),
        generation_state_revision: state.revision,
        journal_evidence_digest: state.journal_evidence_digest.clone(),
    }
}

fn replace_coverage(coverage_entries: &mut Vec<Coverage>, replacement: Coverage) {
    coverage_entries.retain(|entry| entry.generation_id != replacement.generation_id);
    coverage_entries.push(replacement);
    sort_coverage(coverage_entries);
}

fn sort_coverage(coverage_entries: &mut [Coverage]) {
    coverage_entries.sort_by(|left, right| left.generation_id.cmp(&right.generation_id));
}

fn generation_journal_path(root: &Path, generation_id: &str) -> PathBuf {
    root.join("generations")
        .join(format!("{generation_id}.journal"))
}

fn generation_state_path(root: &Path, generation_id: &str) -> PathBuf {
    root.join("generations")
        .join(format!("{generation_id}.state.json"))
}

fn open_coordination_lock(root: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("coordination.lock"))
        .context("opening accounting coordination lock")
}

fn lock_with_timeout(file: &File, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut first_attempt = true;
    loop {
        if !first_attempt && Instant::now() >= deadline {
            bail!("timed out acquiring accounting file lock");
        }
        first_attempt = false;
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                thread::sleep(LOCK_POLL.min(deadline.saturating_duration_since(Instant::now())));
            }
            Err(std::fs::TryLockError::Error(err)) => {
                return Err(err).context("acquiring accounting file lock")
            }
        }
    }
}

fn write_checked<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let checksum = digest(&bytes);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("evidence filename is not valid UTF-8")?;
    let data_staging = path.with_file_name(format!(".{name}.staging"));
    let checksum_path = checksum_path(path)?;
    let checksum_staging = checksum_path.with_file_name(format!(".{name}.sha256.staging"));
    write_synced(&data_staging, &bytes)?;
    write_synced(&checksum_staging, checksum.as_bytes())?;
    fs::rename(&data_staging, path)
        .with_context(|| format!("publishing accounting evidence {}", path.display()))?;
    fs::rename(&checksum_staging, &checksum_path)
        .with_context(|| format!("publishing accounting checksum {}", checksum_path.display()))?;
    sync_directory(path.parent().context("accounting evidence has no parent")?)?;
    Ok(())
}

fn recover_checked_publications(root: &Path) -> Result<()> {
    recover_checked_directory(root, false)?;
    let generations = root.join("generations");
    if generations.is_dir() {
        recover_checked_directory(&generations, true)?;
    }
    Ok(())
}

fn recover_checked_directory(directory: &Path, generations: bool) -> Result<()> {
    let mut staging = BTreeSet::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            bail!("accounting staging filename is not valid UTF-8");
        };
        if name.starts_with('.')
            && name.ends_with(".staging")
            && checked_staging_target(name)
                .is_some_and(|target| checked_target_allowed(target, generations))
        {
            staging.insert(name.to_string());
        }
    }

    for name in staging.clone() {
        if name.ends_with(".sha256.staging") {
            continue;
        }
        let target_name = name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(".staging"))
            .context("invalid accounting data staging filename")?;
        let data_staging = directory.join(&name);
        let checksum_staging_name = format!(".{target_name}.sha256.staging");
        let checksum_staging = directory.join(&checksum_staging_name);
        let target = directory.join(target_name);
        if checksum_staging.exists() {
            let data = fs::read(&data_staging)?;
            if fs::read_to_string(&checksum_staging)? != digest(&data) {
                bail!(
                    "accounting staging checksum mismatch at {}",
                    data_staging.display()
                );
            }
            fs::rename(&data_staging, &target)?;
            fs::rename(&checksum_staging, checksum_path(&target)?)?;
            staging.remove(&checksum_staging_name);
        } else {
            fs::remove_file(&data_staging)?;
        }
        staging.remove(&name);
        sync_directory(directory)?;
    }

    for name in staging {
        let target_name = name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(".sha256.staging"))
            .context("invalid accounting checksum staging filename")?;
        let checksum_staging = directory.join(&name);
        let target = directory.join(target_name);
        if !target.is_file()
            || fs::read_to_string(&checksum_staging)? != digest(&fs::read(&target)?)
        {
            bail!(
                "accounting checksum staging does not match {}",
                target.display()
            );
        }
        fs::rename(&checksum_staging, checksum_path(&target)?)?;
        sync_directory(directory)?;
    }
    Ok(())
}

fn checked_staging_target(name: &str) -> Option<&str> {
    let name = name.strip_prefix('.')?.strip_suffix(".staging")?;
    Some(name.strip_suffix(".sha256").unwrap_or(name))
}

fn checked_target_allowed(name: &str, generations: bool) -> bool {
    if generations {
        return name
            .strip_suffix(".state.json")
            .or_else(|| name.strip_suffix(".replay.json"))
            .is_some_and(|generation_id| !generation_id.is_empty())
            || is_conflict_snapshot_name(name);
    }
    matches!(
        name,
        "audit.json"
            | "installation.json"
            | "incident.json"
            | "coordination.json"
            | "provisioning.json"
            | "adoption.json"
    )
}

fn is_conflict_snapshot_name(name: &str) -> bool {
    conflict_snapshot_generation(name).is_some()
}

fn conflict_snapshot_generation(name: &str) -> Option<&str> {
    conflict_snapshot_identity(name).map(|identity| identity.0)
}

fn conflict_snapshot_identity(name: &str) -> Option<(&str, u64, u64, &str)> {
    let (generation_id, rest) = name.split_once(".conflict.")?;
    let rest = rest.strip_suffix(".json")?;
    let mut parts = rest.split('.');
    let frame_offset = parts.next()?.parse().ok()?;
    let frame_length = parts.next()?.parse().ok()?;
    let frame_digest = parts.next()?;
    if generation_id.is_empty()
        || parts.next().is_some()
        || frame_digest.len() != 64
        || !frame_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some((generation_id, frame_offset, frame_length, frame_digest))
}

fn read_checked<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path)
        .with_context(|| format!("reading accounting evidence {}", path.display()))?;
    let recorded = fs::read_to_string(checksum_path(path)?)
        .with_context(|| format!("reading checksum for {}", path.display()))?;
    if recorded != digest(&bytes) {
        bail!(
            "accounting evidence checksum mismatch at {}",
            path.display()
        );
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing accounting evidence {}", path.display()))
}

fn checksum_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("evidence filename is not valid UTF-8")?;
    Ok(path.with_file_name(format!("{name}.sha256")))
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .with_context(|| format!("creating accounting evidence {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("opening directory {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("syncing directory {}", path.display()))
}

fn digest(bytes: &[u8]) -> String {
    format_digest(Sha256::digest(bytes))
}

fn format_digest(bytes: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        write!(&mut output, "{byte:02x}").expect("write digest");
    }
    output
}

fn require_absolute(path: &Path, name: &str) -> Result<()> {
    if !path.is_absolute() {
        bail!("{name} must be an absolute path: {}", path.display());
    }
    Ok(())
}

fn operator_identity() -> Result<String> {
    ["STEVE_OPERATOR", "USER", "USERNAME"]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .context("set STEVE_OPERATOR to identify the provisioning operator")
}

#[cfg(unix)]
fn effective_actor_identity() -> Result<String> {
    let id = ["/usr/bin/id", "/bin/id"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .context("qualified system id executable is unavailable")?;
    let uid = Command::new(id)
        .arg("-u")
        .output()
        .context("reading effective operator uid")?;
    let account = Command::new(id)
        .arg("-un")
        .output()
        .context("reading effective operator account")?;
    if !uid.status.success() || !account.status.success() {
        bail!("could not derive effective operator identity");
    }
    let uid = String::from_utf8(uid.stdout)?.trim().to_string();
    let account = String::from_utf8(account.stdout)?.trim().to_string();
    if uid.is_empty() || account.is_empty() {
        bail!("effective operator identity is empty");
    }
    Ok(format!("uid:{uid}:account:{account}"))
}

#[cfg(not(unix))]
fn effective_actor_identity() -> Result<String> {
    bail!("accounting disposition identity is unsupported on this platform until qualified")
}

#[cfg(unix)]
fn ensure_platform_supported() -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_platform_supported() -> Result<()> {
    bail!("private accounting roots are unsupported on this platform until qualified")
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("setting private permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory(path: &Path) -> Result<()> {
    bail!(
        "private accounting roots are unsupported on this platform until qualified: {}",
        path.display()
    )
}

#[cfg(unix)]
fn ensure_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "accounting root {} is not private (mode {mode:o})",
            path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn set_readonly_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o400))
        .with_context(|| format!("setting retained backup read-only at {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_kinds_match_exact_loss_and_uncertainty_classes() {
        assert!(disposition_matches("accepted_loss", Some(1), Some(0)));
        assert!(disposition_matches(
            "accepted_uncertainty",
            Some(0),
            Some(1)
        ));
        assert!(disposition_matches(
            "accepted_loss_and_uncertainty",
            Some(1),
            Some(1)
        ));
        assert!(!disposition_matches("accepted_loss", Some(1), None));
        assert!(!disposition_matches("accepted_uncertainty", Some(1), None));
        assert!(!disposition_matches("accepted_uncertainty", Some(0), None));

        let mut uncertain = clear_incident("fixture");
        apply_incident_failure(
            &mut uncertain,
            IncidentCause::PrimaryPersistenceFailedAndJournalUnavailable,
            0,
            1,
        );
        assert_eq!(uncertain.payloads.provisional.unknown, Some(1));
        assert_eq!(
            uncertain.payloads.outcome_totals.unrecoverable_lost,
            Some(0)
        );
    }

    #[test]
    fn verified_new_incident_boundary_discards_prior_publication_ids() {
        let mut incident = clear_incident("fixture");
        incident.state = IncidentState::Acknowledged;
        incident.publication_ids.insert("prior-publication".into());

        apply_incident_failure(
            &mut incident,
            IncidentCause::PrimaryAndJournalUnavailable,
            1,
            0,
        );

        assert!(incident.publication_ids.is_empty());
        assert_eq!(incident.state, IncidentState::Blocked);
    }

    #[test]
    fn acknowledged_sibling_adopts_a_new_incident_lineage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let installation: Installation =
            read_checked(&root.join("installation.json")).expect("installation");

        let disposition = Disposition {
            kind: "accepted_loss".into(),
            actor: "uid:501:account:test".into(),
            at: "2026-09-30T00:00:00Z".into(),
            evidence_ref: "ticket://prior-incident".into(),
        };
        let mut acknowledged = clear_incident(&installation.installation_id);
        acknowledged.revision = 2;
        acknowledged.state = IncidentState::Acknowledged;
        acknowledged.incident_id = Some("prior-incident".into());
        acknowledged.first_observed_at = Some("2026-09-29T00:00:00Z".into());
        acknowledged.cause = Some(IncidentCause::PrimaryAndJournalUnavailable);
        acknowledged.disposition = Some(disposition.clone());
        acknowledged.payloads.outcome_totals.unrecoverable_lost = Some(1);
        acknowledged
            .publication_ids
            .insert("prior-publication".into());
        write_checked(&root.join("incident.json"), &acknowledged).expect("acknowledged incident");
        let audit = vec![AuditRecord {
            format_version: FORMAT_VERSION,
            installation_id: installation.installation_id,
            operation: "acknowledge".into(),
            incident_id: "prior-incident".into(),
            revision: 2,
            previous_revision: 1,
            coverage: Vec::new(),
            disposition: Some(disposition.clone()),
            event_id: None,
            authoritative: None,
            evidence_ref: disposition.evidence_ref.clone(),
            conflict_snapshot_ref: None,
            conflict_snapshot_digest: None,
            final_row: None,
            actor: disposition.actor.clone(),
            at: disposition.at.clone(),
        }];
        write_checked(&root.join("audit.json"), &audit).expect("acknowledgement audit");

        let first = AccountingCoordinator::start(&root, 1).expect("first coordinator");
        let second = AccountingCoordinator::start(&root, 1).expect("second coordinator");
        first
            .latch_incident(IncidentCause::PrimaryAndJournalUnavailable, 1, 0)
            .expect("new incident publication");
        first.flush_incident_publications();
        let durable: Incident =
            read_checked(&root.join("incident.json")).expect("durable new incident");

        let rejected = second
            .with_incident_admission(|| true)
            .expect_err("sibling admitted after durable new incident");
        assert_eq!(rejected["incident_id"], durable.incident_id.unwrap());
        assert_eq!(rejected["state"], "blocked");
    }

    #[test]
    fn acknowledgement_binding_rejects_identity_mismatch_and_clock_skewed_omission() {
        let disposition = Disposition {
            kind: "accepted_loss".into(),
            actor: "uid:501".into(),
            at: "2026-09-29T00:00:00Z".into(),
            evidence_ref: "ticket://accepted-loss".into(),
        };
        let mut incident = clear_incident("installation");
        incident.state = IncidentState::Acknowledged;
        incident.incident_id = Some("incident".into());
        incident.first_observed_at = Some("2026-09-28T00:00:00Z".into());
        incident.cause = Some(IncidentCause::PrimaryAndJournalUnavailable);
        incident.disposition = Some(disposition.clone());
        incident.payloads.outcome_totals.unrecoverable_lost = Some(1);
        let state = GenerationState {
            format_version: FORMAT_VERSION,
            installation_id: "installation".into(),
            generation_id: "generation".into(),
            revision: 2,
            phase: GenerationPhase::Reconciled,
            journal: "/unused/generation.journal".into(),
            journal_length: 0,
            journal_evidence_digest: digest(&[]),
            admitted_count: Some(0),
            worker_completed_count: Some(0),
            journal_synced_count: Some(0),
            database_committed_count: Some(0),
            last_complete_record_boundary: Some(0),
            replay_manifest: None,
            replay_manifest_digest: None,
            updated_at: "2099-01-01T00:00:00Z".into(),
        };
        let mut states = BTreeMap::new();
        states.insert(state.generation_id.clone(), state.clone());
        let mut record = AuditRecord {
            format_version: FORMAT_VERSION,
            installation_id: "installation".into(),
            operation: "acknowledge".into(),
            incident_id: "incident".into(),
            revision: incident.revision,
            previous_revision: incident.revision.saturating_sub(1),
            coverage: vec![coverage(&state)],
            disposition: Some(disposition.clone()),
            event_id: None,
            authoritative: None,
            evidence_ref: disposition.evidence_ref.clone(),
            conflict_snapshot_ref: None,
            conflict_snapshot_digest: None,
            final_row: None,
            actor: "different-actor".into(),
            at: disposition.at.clone(),
        };
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(
            validate_acknowledgement_binding(temp.path(), &record, &incident, &states)
                .expect_err("mismatched top-level audit actor")
                .to_string()
                .contains("identity")
        );

        record.actor = disposition.actor;
        record.coverage.clear();
        assert!(
            validate_acknowledgement_binding(temp.path(), &record, &incident, &states)
                .expect_err("clock-skewed omitted reconciled generation")
                .to_string()
                .contains("omits non-live")
        );
    }

    #[test]
    fn failed_incident_publication_still_blocks_local_admission() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let coordinator = AccountingCoordinator::start(&root, 1).expect("start coordinator");
        let lock = root.join("coordination.lock");
        let unavailable_lock = root.join("coordination.lock.unavailable");
        fs::rename(&lock, &unavailable_lock).expect("make publication unavailable");
        let started = Instant::now();
        coordinator
            .latch_incident(IncidentCause::PrimaryAndJournalUnavailable, 1, 0)
            .expect("queue lost incident publication");
        coordinator
            .latch_incident(
                IncidentCause::PrimaryPersistenceFailedAndJournalUnavailable,
                0,
                1,
            )
            .expect("queue uncertain incident publication");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "request path waited for unavailable durable publication"
        );
        let blocked = coordinator
            .with_incident_admission(|| true)
            .expect_err("local admission reopened during durable publication retry");
        assert_eq!(blocked["state"], "blocked");
        assert_eq!(
            blocked["payloads"]["outcome_totals"]["unrecoverable_lost"],
            1
        );
        assert_eq!(blocked["payloads"]["provisional"]["unknown"], 1);

        fs::rename(unavailable_lock, lock).expect("restore coordination lock");
        coordinator.flush_incident_publications();
        let durable: Incident =
            read_checked(&root.join("incident.json")).expect("durable incident");
        assert_eq!(durable.payloads.outcome_totals.unrecoverable_lost, Some(1));
        assert_eq!(durable.payloads.provisional.unknown, Some(1));
    }

    #[test]
    fn same_base_cross_replica_failures_are_deduplicated_by_publication_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let first = AccountingCoordinator::start(&root, 1).expect("first coordinator");
        let second = AccountingCoordinator::start(&root, 1).expect("second coordinator");

        first
            .latch_incident(IncidentCause::PrimaryAndJournalUnavailable, 1, 0)
            .expect("first publication");
        second
            .latch_incident(IncidentCause::PrimaryAndJournalUnavailable, 1, 0)
            .expect("second publication");
        first.flush_incident_publications();
        second.flush_incident_publications();

        let durable: Incident =
            read_checked(&root.join("incident.json")).expect("durable incident");
        assert_eq!(durable.payloads.outcome_totals.unrecoverable_lost, Some(2));
        assert_eq!(durable.publication_ids.len(), 2);
    }

    #[test]
    fn graceful_shutdown_waits_for_pending_incident_publication() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let coordinator = AccountingCoordinator::start(&root, 1).expect("coordinator");
        let lock = root.join("coordination.lock");
        let unavailable_lock = root.join("coordination.lock.unavailable");
        fs::rename(&lock, &unavailable_lock).expect("make publication unavailable");
        coordinator
            .latch_incident(IncidentCause::PrimaryAndJournalUnavailable, 1, 0)
            .expect("queue incident publication");

        let shutdown = coordinator.clone();
        let stopping = thread::spawn(move || shutdown.begin_shutdown(Duration::from_secs(2)));
        thread::sleep(Duration::from_millis(150));
        assert!(
            !stopping.is_finished(),
            "shutdown skipped pending publication"
        );
        fs::rename(unavailable_lock, lock).expect("restore coordination lock");
        stopping
            .join()
            .expect("shutdown thread")
            .expect("shutdown completed after publication recovery");

        let durable: Incident =
            read_checked(&root.join("incident.json")).expect("durable incident");
        assert_eq!(durable.payloads.outcome_totals.unrecoverable_lost, Some(1));
    }

    #[test]
    fn graceful_shutdown_bounds_unavailable_incident_publication() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let coordinator = AccountingCoordinator::start(&root, 1).expect("coordinator");
        let lock = root.join("coordination.lock");
        let unavailable_lock = root.join("coordination.lock.unavailable");
        fs::rename(&lock, &unavailable_lock).expect("make publication unavailable");
        coordinator
            .latch_incident(IncidentCause::PrimaryAndJournalUnavailable, 1, 0)
            .expect("queue incident publication");

        let started = Instant::now();
        let error = coordinator
            .begin_shutdown(Duration::from_millis(150))
            .expect_err("shutdown exceeded its unavailable publication deadline");
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));

        fs::rename(unavailable_lock, lock).expect("restore coordination lock");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !coordinator
            .incident_publisher_thread
            .lock()
            .expect("incident publisher")
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
        {
            assert!(
                Instant::now() < deadline,
                "incident publisher did not recover"
            );
            thread::sleep(Duration::from_millis(10));
        }
        coordinator
            .incident_publisher_thread
            .lock()
            .expect("incident publisher")
            .take()
            .expect("incident publisher handle")
            .join()
            .expect("incident publisher thread");
    }

    #[test]
    fn graceful_shutdown_bounds_final_ownership_contention() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let coordinator = AccountingCoordinator::start(&root, 1).expect("coordinator");
        let lock = open_coordination_lock(&root).expect("coordination lock");
        lock.lock().expect("hold final ownership publication");

        let started = Instant::now();
        let error = coordinator
            .begin_shutdown(Duration::from_millis(150))
            .expect_err("shutdown exceeded final ownership deadline");
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
        File::unlock(&lock).expect("release final ownership publication");
    }

    #[test]
    fn journal_parser_replays_complete_frames_and_retains_unresolved_bytes() {
        let complete = serde_json::to_vec(&AccountingEvent {
            id: "complete".into(),
            kind: "fixture".into(),
            payload: serde_json::json!({"value": 1}),
            created_at: "2026-09-29T00:00:00Z".into(),
        })
        .expect("complete frame");
        let mut journal = complete.clone();
        journal.extend_from_slice(b"\nnot-json\n{\"id\":\"torn\"");

        let parsed = parse_journal(&journal);

        assert_eq!(parsed.frames.len(), 1);
        assert_eq!(parsed.frames[0].event.id, "complete");
        assert_eq!(parsed.frames[0].offset, 0);
        assert_eq!(parsed.frames[0].length, complete.len() as u64 + 1);
        assert_eq!(parsed.malformed.len(), 1);
        assert_eq!(parsed.malformed[0].offset, complete.len() as u64 + 1);
        assert_eq!(
            parsed.torn_tail.as_ref().map(|tail| tail.length),
            Some(b"{\"id\":\"torn\"".len() as u64)
        );
    }

    #[test]
    fn replay_manifest_recovers_published_conflict_before_retry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let installation: Installation =
            read_checked(&root.join("installation.json")).expect("installation");
        let mut coordination: Coordination =
            read_checked(&root.join("coordination.json")).expect("coordination");
        let generation_id = "fixture-generation";
        let event = AccountingEvent {
            id: "event".into(),
            kind: "fixture".into(),
            payload: serde_json::json!({"value": 1}),
            created_at: "2026-09-29T00:00:00Z".into(),
        };
        let mut journal = serde_json::to_vec(&event).expect("event bytes");
        journal.push(b'\n');
        let parsed = parse_journal(&journal);
        let state = GenerationState {
            format_version: FORMAT_VERSION,
            installation_id: installation.installation_id.clone(),
            generation_id: generation_id.into(),
            revision: 1,
            phase: GenerationPhase::Adopting,
            journal: generation_journal_path(&root, generation_id)
                .display()
                .to_string(),
            journal_length: journal.len() as u64,
            journal_evidence_digest: digest(&journal),
            admitted_count: Some(0),
            worker_completed_count: Some(0),
            journal_synced_count: Some(1),
            database_committed_count: Some(0),
            last_complete_record_boundary: None,
            replay_manifest: None,
            replay_manifest_digest: None,
            updated_at: Utc::now().to_rfc3339(),
        };
        fs::write(generation_journal_path(&root, generation_id), &journal).expect("journal");
        write_checked(&generation_state_path(&root, generation_id), &state).expect("state");
        coordination.revision += 1;
        coordination.coverage.push(coverage(&state));
        write_checked(&root.join("coordination.json"), &coordination).expect("coordination");
        let durability = serde_json::json!({"backend":"sqlite"});
        let manifest_path = replay_manifest_path(&root, generation_id);
        let manifest = load_or_create_replay_manifest(
            &manifest_path,
            &installation,
            &coordination,
            &state,
            &parsed,
            durability.clone(),
        )
        .expect("initial manifest");
        conflict_receipt(
            &root,
            &installation,
            &manifest,
            &parsed.frames[0],
            1,
            crate::storage::BackgroundEventRow {
                id: "event".into(),
                kind: "fixture".into(),
                payload: "{\"value\":2}".into(),
                created_at: "2026-09-29T00:00:00Z".into(),
            },
            "verification",
            "sqlite",
            "serializable",
        )
        .expect("published conflict snapshot");

        let recovered = load_or_create_replay_manifest(
            &manifest_path,
            &installation,
            &coordination,
            &state,
            &parsed,
            durability,
        )
        .expect("recover manifest");

        assert_eq!(recovered.receipts.len(), 1);
        assert_eq!(
            recovered.receipts[0].outcome,
            ReplayOutcome::DuplicateConflict
        );
        assert_eq!(
            read_checked::<Incident>(&root.join("incident.json"))
                .expect("incident before recovered latch")
                .state,
            IncidentState::Clear
        );
        latch_replay_conflict(&root, &installation).expect("recover conflict incident latch");
        latch_replay_conflict(&root, &installation).expect("repeat recovered conflict latch");
        let incident: Incident =
            read_checked(&root.join("incident.json")).expect("recovered conflict incident");
        assert_eq!(incident.state, IncidentState::Blocked);
        assert_eq!(incident.payloads.pending_replay.durable, Some(1));
    }

    #[test]
    fn identical_conflict_frames_have_distinct_artifact_paths() {
        let root = Path::new("/tmp/accounting");
        let line =
            b"{\"id\":\"same\",\"kind\":\"fixture\",\"payload\":{},\"created_at\":\"now\"}\n";
        let parsed = parse_journal(&[line.as_slice(), line.as_slice()].concat());
        assert_eq!(parsed.frames[0].digest, parsed.frames[1].digest);
        let first_frame = &parsed.frames[0];
        let second_frame = &parsed.frames[1];
        let first = conflict_snapshot_path(
            root,
            "generation",
            first_frame.offset,
            first_frame.length,
            &first_frame.digest,
        );
        let second = conflict_snapshot_path(
            root,
            "generation",
            second_frame.offset,
            second_frame.length,
            &second_frame.digest,
        );

        assert_ne!(first, second, "frame position must be part of identity");
    }

    #[test]
    fn database_authority_rejects_distinct_captured_rows_before_mutation() {
        let source = ConflictRowEvidence {
            id: "same-event".into(),
            kind: "fixture".into(),
            payload: "{\"value\":1}".into(),
            created_at: "2026-09-29T00:00:00Z".into(),
        };
        let first_existing = ConflictRowEvidence {
            payload: "{\"value\":2}".into(),
            ..source.clone()
        };
        let second_existing = ConflictRowEvidence {
            payload: "{\"value\":3}".into(),
            ..source.clone()
        };
        let snapshot = |existing: ConflictRowEvidence| RecordedConflictSnapshot {
            format_version: FORMAT_VERSION,
            installation_id: "installation".into(),
            generation_id: "generation".into(),
            journal_offset: 0,
            journal_length: 1,
            journal_record_digest: "journal".into(),
            source: source.clone(),
            existing_row_digest: digest(&serde_json::to_vec(&existing).expect("row JSON")),
            existing,
            verification_id: "verification".into(),
            backend: "sqlite".into(),
            transaction_isolation: "serializable".into(),
            read_evidence: "read".into(),
            observed_at: "2026-09-29T00:00:00Z".into(),
        };
        let mut journal_sides = None;
        let mut database_row = None;
        validate_same_id_conflict_sides(
            "database",
            &snapshot(first_existing.clone()),
            &mut journal_sides,
            &mut database_row,
        )
        .expect("first protected database side");
        let rejected = validate_same_id_conflict_sides(
            "database",
            &snapshot(second_existing),
            &mut journal_sides,
            &mut database_row,
        )
        .expect_err("distinct protected database side");

        assert!(rejected
            .to_string()
            .contains("distinct captured database rows"));
        assert_eq!(database_row, Some(first_existing));
    }

    #[test]
    fn replay_publication_waits_for_coordination_lock() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let installation: Installation =
            read_checked(&root.join("installation.json")).expect("installation");
        let mut coordination: Coordination =
            read_checked(&root.join("coordination.json")).expect("coordination");
        let generation_id = "fixture-generation";
        let journal_path = generation_journal_path(&root, generation_id);
        fs::write(&journal_path, b"").expect("journal");
        let state = GenerationState {
            format_version: FORMAT_VERSION,
            installation_id: installation.installation_id.clone(),
            generation_id: generation_id.into(),
            revision: 1,
            phase: GenerationPhase::Adopting,
            journal: journal_path.display().to_string(),
            journal_length: 0,
            journal_evidence_digest: digest(b""),
            admitted_count: Some(0),
            worker_completed_count: Some(0),
            journal_synced_count: Some(0),
            database_committed_count: Some(0),
            last_complete_record_boundary: None,
            replay_manifest: None,
            replay_manifest_digest: None,
            updated_at: Utc::now().to_rfc3339(),
        };
        write_checked(&generation_state_path(&root, generation_id), &state).expect("state");
        coordination.revision += 1;
        coordination.coverage.push(coverage(&state));
        write_checked(&root.join("coordination.json"), &coordination).expect("coordination");
        let manifest_path = replay_manifest_path(&root, generation_id);
        let mut manifest = load_or_create_replay_manifest(
            &manifest_path,
            &installation,
            &coordination,
            &state,
            &parse_journal(b""),
            serde_json::json!({"backend":"sqlite"}),
        )
        .expect("manifest");
        manifest.retry_exhausted = true;
        let before = fs::read(&manifest_path).expect("manifest bytes");
        let lock = open_coordination_lock(&root).expect("coordination lock");
        lock.lock().expect("hold coordination lock");
        let (started_tx, started_rx) = mpsc::channel();
        let publish_root = root.clone();
        let publish_path = manifest_path.clone();
        let publisher = thread::spawn(move || {
            started_tx.send(()).expect("publisher started");
            publish_replay_manifest(&publish_root, &publish_path, &mut manifest)
        });
        started_rx.recv().expect("publisher ready");
        thread::sleep(Duration::from_millis(20));
        assert_eq!(fs::read(&manifest_path).expect("manifest bytes"), before);
        File::unlock(&lock).expect("release coordination lock");
        publisher
            .join()
            .expect("publisher thread")
            .expect("publish after lock release");
        assert_ne!(fs::read(&manifest_path).expect("manifest bytes"), before);
    }

    #[test]
    fn reconciled_state_publication_repairs_coverage_after_crash() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let installation: Installation =
            read_checked(&root.join("installation.json")).expect("installation");
        let mut coordination: Coordination =
            read_checked(&root.join("coordination.json")).expect("coordination");
        let generation_id = "fixture-generation";
        let journal_path = generation_journal_path(&root, generation_id);
        fs::write(&journal_path, b"").expect("journal");
        let mut state = GenerationState {
            format_version: FORMAT_VERSION,
            installation_id: installation.installation_id.clone(),
            generation_id: generation_id.into(),
            revision: 1,
            phase: GenerationPhase::Adopting,
            journal: journal_path.display().to_string(),
            journal_length: 0,
            journal_evidence_digest: digest(b""),
            admitted_count: Some(0),
            worker_completed_count: Some(0),
            journal_synced_count: Some(0),
            database_committed_count: Some(0),
            last_complete_record_boundary: None,
            replay_manifest: None,
            replay_manifest_digest: None,
            updated_at: Utc::now().to_rfc3339(),
        };
        write_checked(&generation_state_path(&root, generation_id), &state).expect("state");
        coordination.revision += 1;
        coordination.coverage.push(coverage(&state));
        write_checked(&root.join("coordination.json"), &coordination).expect("coordination");
        let manifest_path = replay_manifest_path(&root, generation_id);
        let (ordered_receipt_digest, outcome_counts) = receipt_summary(&[]).expect("summary");
        write_checked(
            &manifest_path,
            &ReplayManifest {
                format_version: FORMAT_VERSION,
                installation_id: installation.installation_id,
                generation_id: generation_id.into(),
                revision: 1,
                source_coordination_revision: coordination.revision,
                source_generation_revision: state.revision,
                journal_length: 0,
                journal_evidence_digest: digest(b""),
                complete_boundary: 0,
                complete_record_count: 0,
                malformed_frames: Vec::new(),
                torn_tail_offset: None,
                torn_tail_length: 0,
                torn_tail_digest: None,
                database_durability: serde_json::json!({"backend":"sqlite"}),
                receipts: Vec::new(),
                ordered_receipt_digest,
                outcome_counts,
                parser_result: "complete".into(),
                sync_result: "synced".into(),
                retry_exhausted: false,
                updated_at: Utc::now().to_rfc3339(),
            },
        )
        .expect("manifest");
        state.revision += 1;
        state.phase = GenerationPhase::Reconciled;
        state.last_complete_record_boundary = Some(0);
        state.replay_manifest = Some(manifest_path.display().to_string());
        state.replay_manifest_digest = Some(digest(&fs::read(&manifest_path).expect("manifest")));
        write_checked(&generation_state_path(&root, generation_id), &state)
            .expect("publish state before coordination");

        recover_reconciled_coverage_gap(&root, &mut coordination, &state)
            .expect("repair expected publication gap");

        let repaired: Coordination =
            read_checked(&root.join("coordination.json")).expect("repaired coordination");
        assert_eq!(
            repaired
                .coverage
                .iter()
                .find(|entry| entry.generation_id == generation_id),
            Some(&coverage(&state))
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platform_rejects_before_creating_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        assert!(AccountingCoordinator::provision(&root).is_err());
        assert!(!root.exists());

        let source = temp.path().join("legacy.jsonl");
        fs::write(&source, b"event\n").expect("legacy source");
        let assertion = temp.path().join("assertion.json");
        assert!(adopt_legacy(&source, &root, &assertion).is_err());
        assert!(!root.exists());
        assert!(!temp.path().join("legacy.jsonl.steve-backup").exists());
    }

    #[test]
    fn replacement_waits_for_active_evidence_publication() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let lock = open_coordination_lock(&root).expect("coordination lock");
        lock.lock().expect("hold publication lock");
        let incident = root.join("incident.json");
        let checksum = checksum_path(&incident).expect("checksum path");
        let incident_staging = root.join("incident.test-staging");
        let checksum_staging = root.join("incident.test-staging.sha256");
        fs::rename(&incident, &incident_staging).expect("stage incident publication");
        fs::rename(&checksum, &checksum_staging).expect("stage checksum publication");

        let start_root = root.clone();
        let replacement = thread::spawn(move || AccountingCoordinator::start(&start_root, 1));
        thread::sleep(Duration::from_millis(25));
        fs::rename(&incident_staging, &incident).expect("publish incident");
        fs::rename(&checksum_staging, &checksum).expect("publish checksum");
        File::unlock(&lock).expect("finish publication");

        replacement
            .join()
            .expect("replacement thread")
            .expect("replacement waits for complete evidence");
    }

    #[test]
    fn start_rejects_snapshot_older_than_freshness_bound() {
        assert_eq!(PRODUCTION_SNAPSHOT_FRESHNESS, Duration::from_millis(100));
        let error = ensure_fresh_with_bound(
            Instant::now() - PRODUCTION_SNAPSHOT_FRESHNESS - Duration::from_millis(1),
            PRODUCTION_SNAPSHOT_FRESHNESS,
        )
        .expect_err("stale ownership snapshot must fail closed");
        assert!(error.to_string().contains("snapshot expired"), "{error:#}");
    }

    #[test]
    fn legacy_adoption_resumes_interrupted_provisioning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("legacy.jsonl");
        fs::write(&source, b"event\n").expect("legacy source");
        let source = fs::canonicalize(source).expect("canonical source");
        let assertion = temp.path().join("maintenance.json");
        fs::write(
            &assertion,
            serde_json::to_vec_pretty(&serde_json::json!({
                "workload_identity": "legacy-steve",
                "host": "fixture-host",
                "source": source,
                "stopped": true,
                "restart_disabled": true,
                "observed_at": "2026-09-29T00:00:00Z",
                "command_or_exported_status": "fixture stop and disable"
            }))
            .expect("assertion JSON"),
        )
        .expect("maintenance assertion");

        let root = temp.path().join("accounting");
        fs::create_dir(&root).expect("partial root");
        set_private_directory(&root).expect("private root");
        fs::create_dir(root.join("generations")).expect("partial generations");
        set_private_directory(&root.join("generations")).expect("private generations");
        File::create(root.join("coordination.lock")).expect("partial coordination lock");

        adopt_legacy(&source, &root, &assertion).expect("resume interrupted adoption provisioning");
        let adoption: Adoption = read_checked(&root.join("adoption.json")).expect("adoption");
        assert_eq!(adoption.state, "pending_reconciliation");
        assert!(generation_journal_path(&root, &adoption.generation_id).is_file());
        assert!(generation_state_path(&root, &adoption.generation_id).is_file());
        let coordination: Coordination =
            read_checked(&root.join("coordination.json")).expect("coordination");
        assert_eq!(coordination.coverage.len(), 1);
    }

    #[test]
    fn empty_legacy_adoption_resumes_after_state_before_coverage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("legacy.jsonl");
        let bytes = b"";
        fs::write(&source, bytes).expect("legacy source");
        let source = fs::canonicalize(source).expect("canonical source");
        let assertion_path = temp.path().join("maintenance.json");
        fs::write(
            &assertion_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "workload_identity": "legacy-steve",
                "host": "fixture-host",
                "source": source,
                "stopped": true,
                "restart_disabled": true,
                "observed_at": "2026-09-29T00:00:00Z",
                "command_or_exported_status": "fixture stop and disable"
            }))
            .expect("assertion JSON"),
        )
        .expect("maintenance assertion");
        let assertion =
            read_maintenance_assertion(&assertion_path, &source).expect("read assertion");
        let backup = source.with_file_name("legacy.jsonl.steve-backup");
        write_or_verify_backup(&backup, bytes).expect("backup");

        let root = temp.path().join("accounting");
        provision_root(
            &root,
            Some(AdoptionSeed {
                state: "importing".into(),
                source: source.display().to_string(),
                backup: backup.display().to_string(),
                source_length: bytes.len() as u64,
                source_digest: digest(bytes),
                maintenance_assertion: assertion,
                generation_id: Uuid::now_v7().to_string(),
                record_outcome: "pending".into(),
            }),
        )
        .expect("provision adoption root");
        let root = fs::canonicalize(root).expect("canonical root");
        let adoption: Adoption = read_checked(&root.join("adoption.json")).expect("adoption");
        let journal_path = generation_journal_path(&root, &adoption.generation_id);
        let mut journal = File::create(&journal_path).expect("published journal");
        journal.write_all(bytes).expect("journal bytes");
        journal.sync_all().expect("journal sync");
        drop(journal);
        sync_directory(&root.join("generations")).expect("generation directory sync");
        let installation: Installation =
            read_checked(&root.join("installation.json")).expect("installation");
        write_checked(
            &generation_state_path(&root, &adoption.generation_id),
            &GenerationState {
                format_version: FORMAT_VERSION,
                installation_id: installation.installation_id,
                generation_id: adoption.generation_id.clone(),
                revision: 1,
                phase: GenerationPhase::Reconciled,
                journal: journal_path.display().to_string(),
                journal_length: 0,
                journal_evidence_digest: digest(bytes),
                admitted_count: Some(0),
                worker_completed_count: Some(0),
                journal_synced_count: Some(0),
                database_committed_count: Some(0),
                last_complete_record_boundary: None,
                replay_manifest: None,
                replay_manifest_digest: None,
                updated_at: Utc::now().to_rfc3339(),
            },
        )
        .expect("publish reconciled state before coverage");

        adopt_legacy(&source, &root, &assertion_path)
            .expect("resume after state published before coverage");
        assert!(generation_state_path(&root, &adoption.generation_id).is_file());
        let coordination: Coordination =
            read_checked(&root.join("coordination.json")).expect("coordination");
        assert_eq!(coordination.coverage.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_backup_resume_restores_immutable_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let backup = temp.path().join("legacy.steve-backup");
        fs::write(&backup, b"event\n").expect("interrupted backup");
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))
            .expect("writable interrupted backup");

        write_or_verify_backup(&backup, b"event\n").expect("resume backup publication");
        assert_eq!(
            fs::metadata(&backup)
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o222,
            0,
            "resumed backup remained writable"
        );
    }

    #[test]
    fn legacy_import_resume_rewrites_partial_staging_from_retained_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let generations = root.join("generations");
        fs::create_dir(&generations).expect("generations");
        let journal = generations.join("fixture.journal");
        let importing = generations.join("fixture.journal.importing");
        fs::write(&importing, b"partial").expect("partial import");

        let file = open_or_publish_import(root, &journal, &importing, b"complete source\n")
            .expect("resume import");
        File::unlock(&file).expect("unlock import");
        assert_eq!(
            fs::read(journal).expect("published journal"),
            b"complete source\n"
        );
        assert!(!importing.exists());
    }

    #[test]
    fn empty_adoption_resumes_after_source_move() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("legacy.jsonl");
        fs::write(&source, b"").expect("empty source");
        let source = fs::canonicalize(source).expect("canonical source");
        let assertion_path = temp.path().join("maintenance.json");
        fs::write(
            &assertion_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "workload_identity": "legacy-steve",
                "host": "fixture-host",
                "source": source,
                "stopped": true,
                "restart_disabled": true,
                "observed_at": "2026-09-29T00:00:00Z",
                "command_or_exported_status": "fixture stop and disable"
            }))
            .expect("assertion JSON"),
        )
        .expect("maintenance assertion");
        let root = temp.path().join("accounting");
        adopt_legacy(&source, &root, &assertion_path).expect("initial empty adoption");

        let adoption_path = root.join("adoption.json");
        let mut adoption: Adoption = read_checked(&adoption_path).expect("adoption");
        adoption.state = "retaining_source".into();
        adoption.source_directory_synced = false;
        write_checked(&adoption_path, &adoption).expect("simulate crash marker");

        adopt_legacy(&source, &root, &assertion_path).expect("resume after source move");
        let adoption: Adoption = read_checked(&adoption_path).expect("completed adoption");
        assert_eq!(adoption.state, "complete");
        assert!(adoption.source_directory_synced);
    }

    #[test]
    fn checked_publication_recovers_each_rename_boundary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("incident.json");
        let checksum = checksum_path(&target).expect("checksum path");
        fs::write(&target, b"old").expect("old evidence");
        fs::write(&checksum, digest(b"old")).expect("old checksum");

        let data_staging = temp.path().join(".incident.json.staging");
        let checksum_staging = temp.path().join(".incident.json.sha256.staging");
        fs::write(&data_staging, b"incomplete").expect("partial staging");
        recover_checked_publications(temp.path()).expect("discard incomplete staging");
        assert!(!data_staging.exists());
        assert_eq!(fs::read(&target).expect("old evidence retained"), b"old");

        fs::write(&data_staging, b"new").expect("staged evidence");
        fs::write(&checksum_staging, digest(b"new")).expect("staged checksum");
        recover_checked_publications(temp.path()).expect("recover before data rename");
        assert_eq!(fs::read(&target).expect("new evidence"), b"new");
        assert_eq!(
            fs::read_to_string(&checksum).expect("new checksum"),
            digest(b"new")
        );

        fs::write(&target, b"newer").expect("published evidence");
        fs::write(&checksum_staging, digest(b"newer")).expect("pending checksum");
        recover_checked_publications(temp.path()).expect("recover after data rename");
        assert_eq!(
            fs::read_to_string(checksum).expect("recovered checksum"),
            digest(b"newer")
        );
    }

    #[test]
    fn invalid_incident_state_fails_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        let path = root.join("incident.json");
        let mut incident: Value = read_checked(&path).expect("incident");
        incident["state"] = Value::String("not_a_state".into());
        write_checked(&path, &incident).expect("write invalid checked incident");

        let error = match AccountingCoordinator::start(&root, 1) {
            Err(error) => error,
            Ok(_) => panic!("invalid incident state must fail closed"),
        };
        assert!(error.to_string().contains("incident"), "{error:#}");
    }

    #[test]
    fn unknown_generation_checksum_artifact_fails_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("accounting");
        AccountingCoordinator::provision(&root).expect("provision");
        fs::write(root.join("generations/unknown.sha256"), digest(b"unknown"))
            .expect("unknown checksum artifact");

        let error = match AccountingCoordinator::start(&root, 1) {
            Err(error) => error,
            Ok(_) => panic!("unknown checksum artifact must fail closed"),
        };
        assert!(error.to_string().contains("unknown"), "{error:#}");
    }
}

#[cfg(not(unix))]
fn ensure_private_directory(path: &Path) -> Result<()> {
    bail!(
        "private accounting roots are unsupported on this platform until qualified: {}",
        path.display()
    )
}

#[cfg(not(unix))]
fn set_readonly_file(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("setting retained backup read-only at {}", path.display()))
}
