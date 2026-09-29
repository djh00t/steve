use crate::{
    accounting::{AccountingCoordinator, AccountingEvent},
    config::Config,
    storage::{DatabasePool, ObjectStorage},
};
use anyhow::Result;
use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tracing::{error, warn};

#[derive(Clone)]
pub struct DeferredQueues {
    accounting: mpsc::Sender<AccountingEvent>,
    accounting_journal: AccountingCoordinator,
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
        let stats = Arc::new(QueueStats::default());
        let accounting_journal = AccountingCoordinator::start(
            std::path::Path::new(&cfg.queues.accounting_journal),
            cfg.queues.accounting_journal_queue,
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

fn spill_accounting_event(
    journal: &AccountingCoordinator,
    stats: &QueueStats,
    event: AccountingEvent,
    reason: &'static str,
) {
    let event_id = event.id.clone();
    match journal.offer(event) {
        Ok(()) => {
            stats.accounting_spilled.fetch_add(1, Ordering::Relaxed);
            warn!(
                event_id = %event_id,
                reason,
                "accounting event spilled to durable journal"
            );
        }
        Err(_event) => {
            stats.accounting_lost.fetch_add(1, Ordering::Relaxed);
            error!(
                event_id = %event_id,
                reason,
                "accounting event could not be queued or journaled"
            );
        }
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
