use crate::{
    accounting::{AccountingCoordinator, AccountingDrainCounts, AccountingEvent, IncidentCause},
    config::Config,
    storage::{DatabasePool, InsertBackgroundEvent, ObjectStorage},
};
use anyhow::Result;
use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tracing::{error, warn};

#[derive(Clone)]
pub struct DeferredQueues {
    accounting: Arc<Mutex<Option<mpsc::Sender<AccountingEvent>>>>,
    accounting_worker: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    accounting_journal: AccountingCoordinator,
    background_db: DatabasePool,
    accounting_retry: AccountingRetryPolicy,
    history: Arc<Mutex<Option<mpsc::Sender<HistoryEvent>>>>,
    history_worker: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    telemetry: Arc<Mutex<Option<mpsc::Sender<TelemetryEvent>>>>,
    telemetry_worker: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    stats: Arc<QueueStats>,
}

#[derive(Default)]
pub struct QueueStats {
    accounting_admitted: AtomicU64,
    accounting_completed: AtomicU64,
    accounting_database_committed: AtomicU64,
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

#[derive(Clone, Copy)]
struct AccountingRetryPolicy {
    operation_timeout: Duration,
    retry_deadline: Duration,
    retry_interval: Duration,
}

impl DeferredQueues {
    pub async fn start(
        cfg: &Config,
        background_db: DatabasePool,
        objects: ObjectStorage,
    ) -> Result<Self> {
        let stats = Arc::new(QueueStats::default());
        let accounting_root = std::path::Path::new(&cfg.queues.accounting_journal);
        let serving = AccountingCoordinator::acquire_serving(accounting_root)?;
        AccountingCoordinator::reconcile_startup(
            accounting_root,
            &background_db,
            std::time::Duration::from_millis(cfg.queues.accounting_operation_timeout_ms),
            std::time::Duration::from_millis(cfg.queues.accounting_retry_deadline_ms),
            std::time::Duration::from_millis(cfg.queues.accounting_retry_interval_ms),
            true,
        )
        .await?;
        let accounting_journal = AccountingCoordinator::start_with_guard(
            accounting_root,
            cfg.queues.accounting_journal_queue,
            serving,
        )?;

        let (accounting_tx, mut accounting_rx) =
            mpsc::channel::<AccountingEvent>(cfg.queues.accounting);
        let (history_tx, mut history_rx) = mpsc::channel::<HistoryEvent>(cfg.queues.history);
        let (telemetry_tx, mut telemetry_rx) =
            mpsc::channel::<TelemetryEvent>(cfg.queues.telemetry);

        let journal_for_worker = accounting_journal.clone();
        let stats_for_worker = stats.clone();
        let accounting_retry = AccountingRetryPolicy {
            operation_timeout: Duration::from_millis(cfg.queues.accounting_operation_timeout_ms),
            retry_deadline: Duration::from_millis(cfg.queues.accounting_retry_deadline_ms),
            retry_interval: Duration::from_millis(cfg.queues.accounting_retry_interval_ms),
        };
        let database_for_worker = background_db.clone();
        let accounting_worker = tokio::spawn(async move {
            while let Some(event) = accounting_rx.recv().await {
                let outcome =
                    insert_accounting_event(&database_for_worker, &event, accounting_retry).await;
                match outcome {
                    InsertBackgroundEvent::Inserted | InsertBackgroundEvent::DuplicateIdentical => {
                        stats_for_worker
                            .accounting_database_committed
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    InsertBackgroundEvent::DuplicateConflict { .. } => {
                        error!(event_id = %event.id, "background accounting content conflict");
                        spill_accounting_event(
                            &journal_for_worker,
                            &stats_for_worker,
                            event,
                            "database_content_conflict",
                            IncidentCause::PrimaryPersistenceFailedAndJournalUnavailable,
                            1,
                            0,
                        );
                    }
                    InsertBackgroundEvent::Failed { error } => {
                        error!(%error, event_id = %event.id, "background accounting write failed");
                        spill_accounting_event(
                            &journal_for_worker,
                            &stats_for_worker,
                            event,
                            "database_write_failed",
                            IncidentCause::PrimaryPersistenceFailedAndJournalUnavailable,
                            1,
                            0,
                        );
                    }
                    InsertBackgroundEvent::Unknown { error } => {
                        error!(%error, event_id = %event.id, "background accounting write outcome is unknown");
                        spill_accounting_event(
                            &journal_for_worker,
                            &stats_for_worker,
                            event,
                            "database_write_unknown",
                            IncidentCause::PrimaryPersistenceFailedAndJournalUnavailable,
                            0,
                            1,
                        );
                    }
                }
                stats_for_worker
                    .accounting_completed
                    .fetch_add(1, Ordering::Release);
            }
        });

        let history_worker = tokio::spawn(async move {
            while let Some(event) = history_rx.recv().await {
                if let Err(err) = objects.put(&event.key, event.data).await {
                    error!(%err, key = %event.key, "background history write failed");
                }
            }
        });

        let telemetry_worker = tokio::spawn(async move {
            while let Some(event) = telemetry_rx.recv().await {
                tracing::info!(kind = event.kind, payload = %event.payload, "deferred telemetry");
            }
        });

        Ok(Self {
            accounting: Arc::new(Mutex::new(Some(accounting_tx))),
            accounting_worker: Arc::new(tokio::sync::Mutex::new(Some(accounting_worker))),
            accounting_journal,
            background_db,
            accounting_retry,
            history: Arc::new(Mutex::new(Some(history_tx))),
            history_worker: Arc::new(tokio::sync::Mutex::new(Some(history_worker))),
            telemetry: Arc::new(Mutex::new(Some(telemetry_tx))),
            telemetry_worker: Arc::new(tokio::sync::Mutex::new(Some(telemetry_worker))),
            stats,
        })
    }

    pub fn accounting(&self, kind: &'static str, payload: Value) {
        let event = AccountingEvent::new(kind, payload);
        self.stats
            .accounting_admitted
            .fetch_add(1, Ordering::Release);
        let sender = self
            .accounting
            .lock()
            .expect("accounting producer")
            .as_ref()
            .cloned();
        let result = match sender {
            Some(sender) => sender.try_send(event),
            None => Err(mpsc::error::TrySendError::Closed(event)),
        };
        match result {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(event))
            | Err(mpsc::error::TrySendError::Closed(event)) => {
                spill_accounting_event(
                    &self.accounting_journal,
                    &self.stats,
                    event,
                    "primary_queue_unavailable",
                    IncidentCause::PrimaryAndJournalUnavailable,
                    1,
                    0,
                );
                self.stats
                    .accounting_completed
                    .fetch_add(1, Ordering::Release);
            }
        }
    }

    pub async fn prepare_shutdown(&self, deadline: Instant) -> Result<()> {
        let coordinator = self.accounting_journal.clone();
        let remaining = remaining(deadline, "preparing accounting shutdown")?;
        tokio::time::timeout(
            remaining,
            tokio::task::spawn_blocking(move || coordinator.prepare_shutdown(deadline)),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out preparing accounting shutdown"))?
        .map_err(|error| anyhow::anyhow!("accounting shutdown task failed: {error}"))??;
        Ok(())
    }

    pub async fn mark_unclean(&self, deadline: Instant) -> Result<()> {
        let coordinator = self.accounting_journal.clone();
        let counts = self.accounting_counts();
        tokio::time::timeout(
            remaining(deadline, "publishing conservative unclean evidence")?,
            tokio::task::spawn_blocking(move || coordinator.mark_unclean(counts)),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out publishing conservative unclean evidence"))?
        .map_err(|error| anyhow::anyhow!("unclean accounting evidence task failed: {error}"))??;
        Ok(())
    }

    pub async fn shutdown(&self, deadline: Instant) -> Result<()> {
        self.accounting.lock().expect("accounting producer").take();
        self.history.lock().expect("history producer").take();
        self.telemetry.lock().expect("telemetry producer").take();
        let evidence = self.mark_unclean(deadline).await;
        join_worker(&self.accounting_worker, deadline, "accounting").await?;
        self.mark_unclean(deadline).await?;
        join_worker(&self.history_worker, deadline, "history").await?;
        join_worker(&self.telemetry_worker, deadline, "telemetry").await?;
        let counts = self.accounting_counts();
        let coordinator = self.accounting_journal.clone();
        let finish =
            tokio::task::spawn_blocking(move || coordinator.finish_shutdown(deadline, counts));
        tokio::time::timeout(
            remaining(deadline, "finalizing accounting ownership")?,
            finish,
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out finalizing accounting ownership"))?
        .map_err(|error| anyhow::anyhow!("accounting shutdown task failed: {error}"))??;
        let remaining = remaining(deadline, "reconciling accounting shutdown")?;
        tokio::time::timeout(
            remaining,
            AccountingCoordinator::reconcile_shutdown(
                self.accounting_journal.root(),
                &self.background_db,
                self.accounting_retry.operation_timeout.min(remaining),
                self.accounting_retry.retry_deadline.min(remaining),
                self.accounting_retry.retry_interval,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out reconciling accounting shutdown"))??;
        evidence?;
        Ok(())
    }

    fn accounting_counts(&self) -> AccountingDrainCounts {
        AccountingDrainCounts {
            admitted: self.stats.accounting_admitted.load(Ordering::Acquire),
            worker_completed: self.stats.accounting_completed.load(Ordering::Acquire),
            database_committed: self
                .stats
                .accounting_database_committed
                .load(Ordering::Acquire),
        }
    }

    pub fn history(&self, key: String, data: Bytes) {
        let sender = self
            .history
            .lock()
            .expect("history producer")
            .as_ref()
            .cloned();
        if sender.is_none_or(|sender| sender.try_send(HistoryEvent { key, data }).is_err()) {
            self.stats.history_dropped.fetch_add(1, Ordering::Relaxed);
            warn!("history queue saturated; payload dropped");
        }
    }

    pub fn telemetry(&self, kind: &'static str, payload: Value) {
        let sender = self
            .telemetry
            .lock()
            .expect("telemetry producer")
            .as_ref()
            .cloned();
        if sender.is_none_or(|sender| sender.try_send(TelemetryEvent { kind, payload }).is_err()) {
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

    pub fn accounting_incident(&self) -> Value {
        self.accounting_journal.public_incident()
    }

    pub fn with_accounting_admission<T>(&self, admit: impl FnOnce() -> T) -> Result<T, Value> {
        self.accounting_journal.with_incident_admission(admit)
    }
}

async fn join_worker(
    worker: &tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    deadline: Instant,
    name: &str,
) -> Result<()> {
    let mut worker = worker.lock().await;
    let Some(handle) = worker.as_mut() else {
        return Ok(());
    };
    tokio::time::timeout(
        remaining(deadline, &format!("stopping {name} worker"))?,
        handle,
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out stopping {name} worker"))?
    .map_err(|error| anyhow::anyhow!("{name} worker failed: {error}"))?;
    worker.take();
    Ok(())
}

fn remaining(deadline: Instant, operation: &str) -> Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        anyhow::bail!("timed out {operation}");
    }
    Ok(remaining)
}

fn spill_accounting_event(
    journal: &AccountingCoordinator,
    stats: &QueueStats,
    event: AccountingEvent,
    reason: &'static str,
    cause: IncidentCause,
    lost: u64,
    unknown: u64,
) {
    let event_id = event.id.clone();
    match journal.offer(event) {
        Ok(()) => {
            stats.accounting_spilled.fetch_add(1, Ordering::Relaxed);
            warn!(
                event_id = %event_id,
                reason,
                "accounting event queued for durable journal"
            );
        }
        Err(_event) => {
            if lost > 0 {
                stats.accounting_lost.fetch_add(lost, Ordering::Relaxed);
            }
            if let Err(err) = journal.latch_incident(cause, lost, unknown) {
                error!(%err, event_id = %event_id, "accounting incident publication failed");
            }
            error!(
                event_id = %event_id,
                reason,
                "accounting event could not be queued or journaled"
            );
        }
    }
}

async fn insert_accounting_event(
    database: &DatabasePool,
    event: &AccountingEvent,
    policy: AccountingRetryPolicy,
) -> InsertBackgroundEvent {
    let Some(deadline) = Instant::now().checked_add(policy.retry_deadline) else {
        return InsertBackgroundEvent::Unknown {
            error: "accounting retry deadline is out of range".into(),
        };
    };
    let payload = event.payload.to_string();
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return InsertBackgroundEvent::Unknown {
                error: "accounting retry deadline exhausted".into(),
            };
        };
        let bound = policy.operation_timeout.min(remaining);
        let outcome = match tokio::time::timeout(
            bound,
            database.insert_background_event(&event.id, &event.kind, &payload, &event.created_at),
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(_) => InsertBackgroundEvent::Unknown {
                error: format!("database operation exceeded {bound:?}"),
            },
        };
        if matches!(
            outcome,
            InsertBackgroundEvent::Inserted
                | InsertBackgroundEvent::DuplicateIdentical
                | InsertBackgroundEvent::DuplicateConflict { .. }
        ) {
            return outcome;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return outcome;
        };
        tokio::time::sleep(policy.retry_interval.min(remaining)).await;
    }
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
