use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
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
const SNAPSHOT_FRESHNESS: Duration = Duration::from_millis(100);

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
    state: String,
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
    phase: String,
    journal: String,
    journal_length: u64,
    journal_evidence_digest: String,
    updated_at: String,
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
    generation_id: String,
    record_outcome: String,
    accepted_evidence_ref: Option<String>,
    updated_at: String,
}

impl AccountingCoordinator {
    pub(crate) fn provision(root: &Path) -> Result<()> {
        require_absolute(root, "accounting root")?;
        if root.exists() && fs::read_dir(root)?.next().is_some() {
            bail!(
                "accounting root {} already contains ownership evidence",
                root.display()
            );
        }
        fs::create_dir_all(root)
            .with_context(|| format!("creating accounting root {}", root.display()))?;
        set_private_directory(root)?;
        let root = fs::canonicalize(root)
            .with_context(|| format!("resolving accounting root {}", root.display()))?;
        let generations = root.join("generations");
        fs::create_dir(&generations)
            .with_context(|| format!("creating {}", generations.display()))?;
        set_private_directory(&generations)?;

        let coordination_lock = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(root.join("coordination.lock"))
            .context("creating accounting coordination lock")?;
        lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;

        let installation_id = Uuid::now_v7().to_string();
        let operator = operator_identity()?;
        write_checked(
            &root.join("installation.json"),
            &Installation {
                format_version: FORMAT_VERSION,
                installation_id: installation_id.clone(),
                root: root.display().to_string(),
            },
        )?;
        write_checked(
            &root.join("incident.json"),
            &Incident {
                format_version: FORMAT_VERSION,
                installation_id: installation_id.clone(),
                revision: 1,
                state: "clear".into(),
            },
        )?;
        write_checked(
            &root.join("coordination.json"),
            &Coordination {
                format_version: FORMAT_VERSION,
                installation_id: installation_id.clone(),
                revision: 1,
                coverage: Vec::new(),
            },
        )?;
        write_checked(
            &root.join("provisioning.json"),
            &Provisioning {
                format_version: FORMAT_VERSION,
                installation_id,
                operator,
                root: root.display().to_string(),
                inventory: Vec::new(),
                revision: 1,
                created_at: Utc::now().to_rfc3339(),
                files_synced: true,
                directory_synced: true,
            },
        )?;
        sync_directory(&root)?;
        File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
        Ok(())
    }

    pub(crate) fn adopt_legacy(source: &Path, root: &Path) -> Result<String> {
        require_absolute(source, "legacy accounting journal")?;
        require_absolute(root, "accounting root")?;
        if source == root || source.starts_with(root) || root.starts_with(source) {
            bail!("legacy source and accounting root must be distinct paths");
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
                "legacy journal {} is busy; adoption requires all writers stopped",
                source.display()
            )
        })?;

        let source = fs::canonicalize(source)
            .with_context(|| format!("resolving legacy journal {}", source.display()))?;
        let mut bytes = Vec::new();
        (&source_file)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading legacy journal {}", source.display()))?;
        let source_digest = digest(&bytes);
        let backup = source.with_file_name(format!(
            "{}.steve-backup",
            source
                .file_name()
                .and_then(|name| name.to_str())
                .context("legacy journal name is not valid UTF-8")?
        ));
        write_or_verify_backup(&backup, &bytes)?;

        if !root.exists() {
            Self::provision(root)?;
        }
        let (root, installation) = load_provisioned_root(root)?;
        let coordination_lock = open_coordination_lock(&root)?;
        lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
        let mut coordination: Coordination = read_checked(&root.join("coordination.json"))?;
        let adoption_path = root.join("adoption.json");

        let mut adoption = if adoption_path.exists() {
            let existing: Adoption = read_checked(&adoption_path)?;
            ensure_adoption_matches(&existing, &installation, &source, &backup, &bytes)?;
            existing
        } else {
            if fs::read_dir(root.join("generations"))?.next().is_some() {
                bail!("accounting root contains generations without adoption evidence");
            }
            let record = Adoption {
                format_version: FORMAT_VERSION,
                installation_id: installation.installation_id.clone(),
                state: "importing".into(),
                source: source.display().to_string(),
                backup: backup.display().to_string(),
                source_length: bytes.len() as u64,
                source_digest: source_digest.clone(),
                generation_id: Uuid::now_v7().to_string(),
                record_outcome: "pending".into(),
                accepted_evidence_ref: None,
                updated_at: Utc::now().to_rfc3339(),
            };
            write_checked(&adoption_path, &record)?;
            record
        };

        let journal_path = generation_journal_path(&root, &adoption.generation_id);
        let state_path = generation_state_path(&root, &adoption.generation_id);
        if journal_path.exists() {
            let imported = fs::read(&journal_path)
                .with_context(|| format!("reading imported journal {}", journal_path.display()))?;
            if imported != bytes {
                bail!("imported journal does not match retained legacy source");
            }
            let state: GenerationState = read_checked(&state_path)?;
            if state.journal_evidence_digest != source_digest
                || state.journal_length != bytes.len() as u64
            {
                bail!("imported generation evidence does not match retained source");
            }
        } else {
            let staging = journal_path.with_extension("journal.staging");
            let mut journal = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&staging)
                .with_context(|| format!("creating import staging {}", staging.display()))?;
            journal.lock().context("locking import staging journal")?;
            journal.write_all(&bytes)?;
            journal.sync_all()?;
            fs::rename(&staging, &journal_path).with_context(|| {
                format!(
                    "publishing imported journal {} as {}",
                    staging.display(),
                    journal_path.display()
                )
            })?;
            sync_directory(&root.join("generations"))?;
            let state = GenerationState {
                format_version: FORMAT_VERSION,
                installation_id: installation.installation_id.clone(),
                generation_id: adoption.generation_id.clone(),
                revision: 1,
                phase: "adopting".into(),
                journal: journal_path.display().to_string(),
                journal_length: bytes.len() as u64,
                journal_evidence_digest: source_digest.clone(),
                updated_at: Utc::now().to_rfc3339(),
            };
            write_checked(&state_path, &state)?;
            coordination.revision += 1;
            coordination.coverage.push(coverage(&state));
            sort_coverage(&mut coordination.coverage);
            write_checked(&root.join("coordination.json"), &coordination)?;
            File::unlock(&journal).context("unlocking imported generation")?;
        }

        adoption.state = "pending_reconciliation".into();
        adoption.record_outcome = "pending_reconciliation".into();
        adoption.updated_at = Utc::now().to_rfc3339();
        write_checked(&adoption_path, &adoption)?;
        sync_directory(&root)?;
        File::unlock(&coordination_lock).context("unlocking accounting coordination")?;
        File::unlock(&source_file).context("unlocking legacy journal")?;
        Ok(format!(
            "legacy accounting journal imported; pending reconciliation for generation {}",
            adoption.generation_id
        ))
    }

    pub(crate) fn start(root: &Path, capacity: usize) -> Result<Self> {
        let (root, installation) = load_provisioned_root(root)?;
        if root.join("adoption.json").exists() {
            let adoption: Adoption = read_checked(&root.join("adoption.json"))?;
            if adoption.state != "complete" {
                bail!(
                    "legacy accounting adoption is {}; pending reconciliation",
                    adoption.state
                );
            }
        }

        let coordination_lock = open_coordination_lock(&root)?;
        lock_with_timeout(&coordination_lock, LOCK_TIMEOUT)?;
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
            phase: "active".into(),
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
        sync_directory(&root)?;
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
        let mut coordination: Coordination = read_checked(&self.root.join("coordination.json"))?;
        let mut state = self.state.lock().expect("accounting generation state");
        if state.generation_id != self.generation_id || state.phase != "active" {
            bail!("accounting generation is not active");
        }
        let persisted: GenerationState =
            read_checked(&generation_state_path(&self.root, &self.generation_id))?;
        if persisted.revision != state.revision {
            bail!("accounting generation revision changed during shutdown");
        }
        state.revision += 1;
        state.phase = "draining".into();
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
    let started = Instant::now();
    let (root, installation) = load_provisioned_root(root)?;
    let lock = open_coordination_lock(&root)?;
    lock_with_timeout(&lock, LOCK_TIMEOUT)?;
    let coordination: Coordination = read_checked(&root.join("coordination.json"))?;
    verify_generations(&root, &installation, &coordination)?;
    if started.elapsed() > SNAPSHOT_FRESHNESS {
        bail!("accounting ownership snapshot expired before completion");
    }
    File::unlock(&lock).context("unlocking accounting coordination")?;
    Ok(AccountingSnapshot {
        installation_id: coordination.installation_id,
        revision: coordination.revision,
        coverage: coordination.coverage,
    })
}

fn load_provisioned_root(root: &Path) -> Result<(PathBuf, Installation)> {
    require_absolute(root, "accounting root")?;
    if !root.exists() {
        bail!("accounting root {} is not provisioned", root.display());
    }
    let root = fs::canonicalize(root)
        .with_context(|| format!("resolving accounting root {}", root.display()))?;
    ensure_private_directory(&root)?;
    verify_root_inventory(&root)?;
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
    if provisioning.root != installation.root || provisioning.inventory != Vec::<String>::new() {
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
    Ok((root, installation))
}

fn verify_generations(
    root: &Path,
    installation: &Installation,
    coordination: &Coordination,
) -> Result<BTreeMap<String, GenerationState>> {
    let generation_root = root.join("generations");
    let mut states = BTreeMap::new();
    let mut journals = BTreeSet::new();
    for entry in fs::read_dir(&generation_root)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("generation filename is not valid UTF-8")?;
        if name.ends_with(".sha256") {
            continue;
        }
        if let Some(id) = name.strip_suffix(".state.json") {
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
                if matches!(state.phase.as_str(), "active" | "draining" | "adopting") {
                    bail!(
                        "generation {} has unresolved {} evidence",
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
                if state.phase != "active" {
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
) -> Result<()> {
    if adoption.format_version != FORMAT_VERSION
        || adoption.installation_id != installation.installation_id
        || adoption.source != source.display().to_string()
        || adoption.backup != backup.display().to_string()
        || adoption.source_length != bytes.len() as u64
        || adoption.source_digest != digest(bytes)
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

fn write_or_verify_backup(path: &Path, bytes: &[u8]) -> Result<()> {
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
            set_readonly_file(path)?;
            sync_directory(path.parent().context("legacy backup has no parent")?)?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(path)? != bytes {
                bail!("existing legacy backup does not match source");
            }
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
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    bail!("timed out acquiring accounting file lock");
                }
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
    let suffix = Uuid::now_v7();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("evidence filename is not valid UTF-8")?;
    let data_staging = path.with_file_name(format!(".{name}.{suffix}.staging"));
    let checksum_path = checksum_path(path)?;
    let checksum_staging = checksum_path.with_file_name(format!(".{name}.{suffix}.sha256.staging"));
    write_synced_new(&data_staging, &bytes)?;
    write_synced_new(&checksum_staging, checksum.as_bytes())?;
    fs::rename(&data_staging, path)
        .with_context(|| format!("publishing accounting evidence {}", path.display()))?;
    fs::rename(&checksum_staging, &checksum_path)
        .with_context(|| format!("publishing accounting checksum {}", checksum_path.display()))?;
    sync_directory(path.parent().context("accounting evidence has no parent")?)?;
    Ok(())
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

fn write_synced_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
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
fn set_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("setting private permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<()> {
    Ok(())
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

#[cfg(not(unix))]
fn ensure_private_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn set_readonly_file(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("setting retained backup read-only at {}", path.display()))
}
