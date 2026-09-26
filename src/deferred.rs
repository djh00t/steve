use crate::{
    config::Config,
    storage::{DatabasePool, ObjectStorage},
};
use anyhow::{Context, Result};
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self as std_mpsc, SyncSender},
        Arc,
    },
    thread,
};
use tokio::sync::mpsc;
use tracing::{error, warn};
use uuid::Uuid;

#[derive(Clone)]
pub struct DeferredQueues {
    accounting: mpsc::Sender<AccountingEvent>,
    accounting_journal: SyncSender<AccountingEvent>,
    history: mpsc::Sender<HistoryEvent>,
    telemetry: mpsc::Sender<TelemetryEvent>,
    stats: Arc<QueueStats>,
}

#[derive(Default)]
pub struct QueueStats {
    pub accounting_spilled: AtomicU64,
    pub accounting_lost: AtomicU64,
    pub history_dropped: AtomicU64,
    pub telemetry_dropped: AtomicU64,
}

#[derive(Clone, Debug, Serialize)]
pub struct QueueSnapshot {
    pub accounting_spilled: u64,
    pub accounting_lost: u64,
    pub history_dropped: u64,
    pub telemetry_dropped: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AccountingEvent {
    id: String,
    kind: String,
    payload: Value,
    created_at: String,
}

impl AccountingEvent {
    fn new(kind: &'static str, payload: Value) -> Self {
        Self {
            id: Uuid::now_v7().to_string(),
            kind: kind.to_string(),
            payload,
            created_at: Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug)]
struct TelemetryEvent {
    kind: &'static str,
    payload: Value,
}

#[derive(Debug)]
struct HistoryEvent {
    key: String,
    data: Bytes,
}

impl DeferredQueues {
    pub async fn start(
        cfg: &Config,
        background_db: DatabasePool,
        objects: ObjectStorage,
    ) -> Result<Self> {
        let journal_path = PathBuf::from(&cfg.queues.accounting_journal);
        replay_accounting_journal(&journal_path, &background_db).await?;

        let stats = Arc::new(QueueStats::default());
        let accounting_journal = start_accounting_journal(
            journal_path,
            cfg.queues.accounting_journal_queue,
            stats.clone(),
        )?;

        let (accounting_tx, mut accounting_rx) =
            mpsc::channel::<AccountingEvent>(cfg.queues.accounting);
        let (history_tx, mut history_rx) = mpsc::channel::<HistoryEvent>(cfg.queues.history);
        let (telemetry_tx, mut telemetry_rx) =
            mpsc::channel::<TelemetryEvent>(cfg.queues.telemetry);

        let journal_for_worker = accounting_journal.clone();
        let stats_for_worker = stats.clone();
        tokio::spawn(async move {
            while let Some(event) = accounting_rx.recv().await {
                let result = background_db
                    .insert_background_event(
                        &event.id,
                        &event.kind,
                        &event.payload.to_string(),
                        &event.created_at,
                    )
                    .await;
                if let Err(err) = result {
                    error!(%err, event_id = %event.id, "background accounting write failed");
                    spill_accounting_event(
                        &journal_for_worker,
                        &stats_for_worker,
                        event,
                        "database_write_failed",
                    );
                }
            }
        });

        tokio::spawn(async move {
            while let Some(event) = history_rx.recv().await {
                if let Err(err) = objects.put(&event.key, event.data).await {
                    error!(%err, key = %event.key, "background history write failed");
                }
            }
        });

        tokio::spawn(async move {
            while let Some(event) = telemetry_rx.recv().await {
                tracing::info!(kind = event.kind, payload = %event.payload, "deferred telemetry");
            }
        });

        Ok(Self {
            accounting: accounting_tx,
            accounting_journal,
            history: history_tx,
            telemetry: telemetry_tx,
            stats,
        })
    }

    pub fn accounting(&self, kind: &'static str, payload: Value) {
        let event = AccountingEvent::new(kind, payload);
        match self.accounting.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(event))
            | Err(mpsc::error::TrySendError::Closed(event)) => {
                spill_accounting_event(
                    &self.accounting_journal,
                    &self.stats,
                    event,
                    "primary_queue_unavailable",
                );
            }
        }
    }

    pub fn history(&self, key: String, data: Bytes) {
        if self.history.try_send(HistoryEvent { key, data }).is_err() {
            self.stats.history_dropped.fetch_add(1, Ordering::Relaxed);
            warn!("history queue saturated; payload dropped");
        }
    }

    pub fn telemetry(&self, kind: &'static str, payload: Value) {
        if self
            .telemetry
            .try_send(TelemetryEvent { kind, payload })
            .is_err()
        {
            self.stats.telemetry_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            accounting_spilled: self.stats.accounting_spilled.load(Ordering::Relaxed),
            accounting_lost: self.stats.accounting_lost.load(Ordering::Relaxed),
            history_dropped: self.stats.history_dropped.load(Ordering::Relaxed),
            telemetry_dropped: self.stats.telemetry_dropped.load(Ordering::Relaxed),
        }
    }
}

fn start_accounting_journal(
    path: PathBuf,
    capacity: usize,
    stats: Arc<QueueStats>,
) -> Result<SyncSender<AccountingEvent>> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating journal directory {}", parent.display()))?;
        }
    }

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening accounting journal {}", path.display()))?;
    let (tx, rx) = std_mpsc::sync_channel::<AccountingEvent>(capacity.max(1));

    thread::Builder::new()
        .name("steve-accounting-journal".into())
        .spawn(move || {
            let mut writer = BufWriter::new(file);
            while let Ok(event) = rx.recv() {
                let write_result = serde_json::to_writer(&mut writer, &event)
                    .and_then(|_| {
                        writer
                            .write_all(b"\n")
                            .map_err(serde_json::Error::io)
                    })
                    .and_then(|_| writer.flush().map_err(serde_json::Error::io));

                if let Err(err) = write_result {
                    stats.accounting_lost.fetch_add(1, Ordering::Relaxed);
                    error!(
                        %err,
                        event_id = %event.id,
                        "accounting journal write failed"
                    );
                }
            }

            if let Err(err) = writer.flush() {
                error!(%err, "accounting journal final flush failed");
            }
        })
        .context("spawning accounting journal writer")?;

    Ok(tx)
}

fn spill_accounting_event(
    journal: &SyncSender<AccountingEvent>,
    stats: &QueueStats,
    event: AccountingEvent,
    reason: &'static str,
) {
    let event_id = event.id.clone();
    match journal.try_send(event) {
        Ok(()) => {
            stats.accounting_spilled.fetch_add(1, Ordering::Relaxed);
            warn!(
                event_id = %event_id,
                reason,
                "accounting event spilled to durable journal"
            );
        }
        Err(err) => {
            stats.accounting_lost.fetch_add(1, Ordering::Relaxed);
            error!(
                %err,
                event_id = %event_id,
                reason,
                "accounting event could not be queued or journaled"
            );
        }
    }
}

async fn replay_accounting_journal(path: &Path, db: &DatabasePool) -> Result<()> {
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("reading accounting journal {}", path.display()));
        }
    };

    let mut replayed = 0_u64;
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        let event: AccountingEvent = serde_json::from_str(line).with_context(|| {
            format!(
                "parsing accounting journal {} line {}",
                path.display(),
                index + 1
            )
        })?;

        db.insert_background_event(
            &event.id,
            &event.kind,
            &event.payload.to_string(),
            &event.created_at,
        )
        .await?;
        replayed += 1;
    }

    if replayed > 0 {
        tokio::fs::write(path, b"")
            .await
            .with_context(|| format!("truncating accounting journal {}", path.display()))?;
        tracing::info!(
            event = "accounting_journal_replayed",
            entries = replayed,
            path = %path.display(),
            "replayed durable accounting journal"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounting_event_round_trips_json() {
        let event = AccountingEvent::new("test", serde_json::json!({"value": 42}));
        let encoded = serde_json::to_string(&event).expect("serialize");
        let decoded: AccountingEvent = serde_json::from_str(&encoded).expect("deserialize");

        assert_eq!(decoded.id, event.id);
        assert_eq!(decoded.kind, "test");
        assert_eq!(decoded.payload, serde_json::json!({"value": 42}));
    }
}
