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
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex,
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

#[derive(Clone)]
pub(crate) struct AccountingCoordinator {
    root: PathBuf,
    generation_id: String,
    journal: SyncSender<AccountingEvent>,
    accepting: Arc<AtomicBool>,
    state: Arc<Mutex<GenerationState>>,
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

#[derive(Debug, Deserialize, Serialize)]
struct Incident {
    format_version: u64,
    installation_id: String,
    revision: u64,
    state: IncidentState,
    incident_id: Option<String>,
    first_observed_at: Option<String>,
    cause: Option<IncidentCause>,
    disposition: Option<Value>,
    payloads: IncidentPayloads,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum IncidentState {
    Clear,
    Blocked,
    Unreconciled,
    Acknowledged,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum IncidentCause {
    PrimaryAndJournalUnavailable,
    PrimaryPersistenceFailedAndJournalUnavailable,
    JournalWriteFailed,
    PriorIncidentUnreconciled,
    ReplayContentConflict,
}

#[derive(Debug, Deserialize, Serialize)]
struct IncidentPayloads {
    pending_replay: PendingReplay,
    provisional: Provisional,
    outcome_totals: OutcomeTotals,
}

#[derive(Debug, Deserialize, Serialize)]
struct PendingReplay {
    volatile: Option<u64>,
    durable: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Provisional {
    unknown: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OutcomeTotals {
    reconciled: Option<u64>,
    unrecoverable_lost: Option<u64>,
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

#[derive(Debug, Deserialize, Serialize)]
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
    }
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

    pub(crate) fn start(root: &Path, capacity: usize) -> Result<Self> {
        let root = resolve_accounting_root(root)?;
        let coordination_lock = open_coordination_lock(&root)?;
        lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
        let started = Instant::now();
        recover_checked_publications(&root)?;
        let installation = load_provisioned_root_locked(&root)?;
        if root.join("adoption.json").exists() {
            let adoption: Adoption = read_checked(&root.join("adoption.json"))?;
            if adoption.state != "complete" {
                bail!(
                    "legacy accounting adoption is {}; pending reconciliation",
                    adoption.state
                );
            }
        }

        cleanup_empty_staging(&root)?;
        let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
        let states = verify_generations(&root, &installation, &coordination)?;
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

        let state = GenerationState {
            format_version: FORMAT_VERSION,
            installation_id: installation.installation_id.clone(),
            generation_id: generation_id.clone(),
            revision: 1,
            phase: GenerationPhase::Active,
            journal: journal_path.display().to_string(),
            journal_length: 0,
            journal_evidence_digest: digest(&[]),
            updated_at: Utc::now().to_rfc3339(),
        };
        write_checked(&generation_state_path(&root, &generation_id), &state)?;
        coordination.revision += 1;
        coordination.coverage = states.values().map(coverage).collect();
        coordination.coverage.push(coverage(&state));
        sort_coverage(&mut coordination.coverage);
        write_checked(&root.join("coordination.json"), &coordination)?;
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
        let (journal, receiver) = mpsc::sync_channel::<AccountingEvent>(capacity.max(1));
        start_writer(root.clone(), journal_file, receiver, state.clone())?;
        Ok(Self {
            root,
            generation_id,
            journal,
            accepting: Arc::new(AtomicBool::new(true)),
            state,
        })
    }

    pub(crate) fn offer(&self, event: AccountingEvent) -> Result<(), AccountingEvent> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(event);
        }
        match self.journal.try_send(event) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(event) | TrySendError::Disconnected(event)) => Err(event),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn snapshot(&self) -> Result<AccountingSnapshot> {
        load_snapshot(&self.root)
    }

    #[allow(dead_code)]
    pub(crate) fn begin_shutdown(&self) -> Result<AccountingSnapshot> {
        self.accepting.store(false, Ordering::Release);
        let lock = open_coordination_lock(&self.root)?;
        lock_with_timeout(&lock, LOCK_TIMEOUT)?;
        recover_checked_publications(&self.root)?;
        let mut coordination: Coordination = read_checked(&self.root.join("coordination.json"))?;
        let mut state = self.state.lock().expect("accounting generation state");
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
        coordination.revision += 1;
        replace_coverage(&mut coordination.coverage, coverage(&state));
        write_checked(&self.root.join("coordination.json"), &coordination)?;
        File::unlock(&lock).context("unlocking accounting coordination")?;
        Ok(AccountingSnapshot {
            installation_id: coordination.installation_id,
            revision: coordination.revision,
            coverage: coordination.coverage,
        })
    }
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
    let state = if state_path.exists() {
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
        let retained = retained_source_path(&source)?;
        adoption.state = "retaining_source".into();
        adoption.record_outcome = "empty_vacuous".into();
        adoption.accepted_evidence_ref = Some(format!("sha256:{}", digest(&bytes)));
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
    if adoption.installation_id != installation.installation_id
        || !matches!(adoption.state.as_str(), "retaining_source" | "complete")
        || adoption.source != source.display().to_string()
        || adoption.source_length != 0
        || adoption.source_digest != digest(&[])
        || adoption.maintenance_assertion != assertion
        || adoption.record_outcome != "empty_vacuous"
        || adoption.accepted_evidence_ref != Some(format!("sha256:{}", digest(&[])))
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
    receiver: mpsc::Receiver<AccountingEvent>,
    state: Arc<Mutex<GenerationState>>,
) -> Result<()> {
    thread::Builder::new()
        .name("steve-accounting-journal".into())
        .spawn(move || {
            let mut hasher = Sha256::new();
            while let Ok(event) = receiver.recv() {
                if let Err(err) = append_event(&root, &mut journal, &mut hasher, &state, &event) {
                    tracing::error!(%err, event_id = %event.id, "accounting journal write failed");
                }
            }
            if let Err(err) = journal.sync_all() {
                tracing::error!(%err, "accounting journal final sync failed");
            }
        })
        .context("spawning accounting journal writer")?;
    Ok(())
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
    if !root.join("coordination.lock").is_file() || !root.join("generations").is_dir() {
        bail!("accounting ownership evidence is incomplete");
    }
    validate_incident(&incident)?;
    Ok(installation)
}

fn validate_incident(incident: &Incident) -> Result<()> {
    if incident.state == IncidentState::Clear
        && (incident.incident_id.is_some()
            || incident.first_observed_at.is_some()
            || incident.cause.is_some()
            || incident.disposition.is_some()
            || incident.payloads.pending_replay.volatile != Some(0)
            || incident.payloads.pending_replay.durable != Some(0)
            || incident.payloads.provisional.unknown != Some(0)
            || incident.payloads.outcome_totals.reconciled != Some(0)
            || incident.payloads.outcome_totals.unrecoverable_lost != Some(0))
    {
        bail!("clear accounting incident evidence is inconsistent");
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
    let generation_root = root.join("generations");
    let mut states = BTreeMap::new();
    let mut state_checksums = BTreeSet::new();
    let mut journals = BTreeSet::new();
    for entry in fs::read_dir(&generation_root)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("generation filename is not valid UTF-8")?;
        if let Some(id) = name.strip_suffix(".state.json.sha256") {
            state_checksums.insert(id.to_string());
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
                ) {
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
        "coordination.lock".to_string(),
        "coordination.json".to_string(),
        "coordination.json.sha256".to_string(),
        "generations".to_string(),
        "incident.json".to_string(),
        "incident.json.sha256".to_string(),
        "installation.json".to_string(),
        "installation.json.sha256".to_string(),
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
            .is_some_and(|generation_id| !generation_id.is_empty());
    }
    matches!(
        name,
        "installation.json"
            | "incident.json"
            | "coordination.json"
            | "provisioning.json"
            | "adoption.json"
    )
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
