use crate::{config::Config, storage::ObjectStorage};
use bytes::Bytes;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use sqlx::AnyPool;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tracing::{error, warn};
use uuid::Uuid;

#[derive(Clone)]
pub struct DeferredQueues {
    accounting: mpsc::Sender<Event>,
    history: mpsc::Sender<HistoryEvent>,
    telemetry: mpsc::Sender<Event>,
    stats: Arc<QueueStats>,
}

#[derive(Default)]
pub struct QueueStats {
    pub accounting_dropped: AtomicU64,
    pub history_dropped: AtomicU64,
    pub telemetry_dropped: AtomicU64,
}

#[derive(Clone, Debug, Serialize)]
pub struct QueueSnapshot {
    pub accounting_dropped: u64,
    pub history_dropped: u64,
    pub telemetry_dropped: u64,
}

#[derive(Debug)]
struct Event {
    kind: &'static str,
    payload: Value,
}

#[derive(Debug)]
struct HistoryEvent {
    key: String,
    data: Bytes,
}

impl DeferredQueues {
    pub fn start(cfg: &Config, background_db: AnyPool, objects: ObjectStorage) -> Self {
        let (accounting_tx, mut accounting_rx) = mpsc::channel::<Event>(cfg.queues.accounting);
        let (history_tx, mut history_rx) = mpsc::channel::<HistoryEvent>(cfg.queues.history);
        let (telemetry_tx, mut telemetry_rx) = mpsc::channel::<Event>(cfg.queues.telemetry);
        let stats = Arc::new(QueueStats::default());

        tokio::spawn(async move {
            while let Some(event) = accounting_rx.recv().await {
                let result = sqlx::query(
                    "INSERT INTO steve_background_events(id, kind, payload, created_at) VALUES (?, ?, ?, ?)",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(event.kind)
                .bind(event.payload.to_string())
                .bind(Utc::now().to_rfc3339())
                .execute(&background_db)
                .await;
                if let Err(err) = result {
                    error!(%err, "background accounting write failed");
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

        Self {
            accounting: accounting_tx,
            history: history_tx,
            telemetry: telemetry_tx,
            stats,
        }
    }

    pub fn accounting(&self, kind: &'static str, payload: Value) {
        if self.accounting.try_send(Event { kind, payload }).is_err() {
            self.stats
                .accounting_dropped
                .fetch_add(1, Ordering::Relaxed);
            error!("accounting queue saturated; event was not persisted");
        }
    }

    pub fn history(&self, key: String, data: Bytes) {
        if self.history.try_send(HistoryEvent { key, data }).is_err() {
            self.stats
                .history_dropped
                .fetch_add(1, Ordering::Relaxed);
            warn!("history queue saturated; payload dropped");
        }
    }

    pub fn telemetry(&self, kind: &'static str, payload: Value) {
        if self.telemetry.try_send(Event { kind, payload }).is_err() {
            self.stats
                .telemetry_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            accounting_dropped: self.stats.accounting_dropped.load(Ordering::Relaxed),
            history_dropped: self.stats.history_dropped.load(Ordering::Relaxed),
            telemetry_dropped: self.stats.telemetry_dropped.load(Ordering::Relaxed),
        }
    }
}
