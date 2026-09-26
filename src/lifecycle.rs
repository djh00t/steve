use serde::Serialize;
use std::sync::{
    atomic::{AtomicU64, AtomicU8, Ordering},
    Arc,
};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Ready,
    Draining,
    Stopped,
}

#[derive(Clone)]
pub struct Lifecycle {
    phase: Arc<AtomicU8>,
    inflight: Arc<AtomicU64>,
    changed: Arc<Notify>,
}

pub struct InflightGuard {
    lifecycle: Lifecycle,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            phase: Arc::new(AtomicU8::new(0)),
            inflight: Arc::new(AtomicU64::new(0)),
            changed: Arc::new(Notify::new()),
        }
    }

    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Acquire) {
            1 => Phase::Ready,
            2 => Phase::Draining,
            3 => Phase::Stopped,
            _ => Phase::Starting,
        }
    }

    pub fn is_ready(&self) -> bool { self.phase() == Phase::Ready }
    pub fn inflight(&self) -> u64 { self.inflight.load(Ordering::Acquire) }

    pub fn ready(&self) {
        self.phase.store(1, Ordering::Release);
        self.changed.notify_waiters();
    }

    pub fn drain(&self) {
        self.phase.store(2, Ordering::Release);
        self.changed.notify_waiters();
    }

    pub fn stopped(&self) {
        self.phase.store(3, Ordering::Release);
        self.changed.notify_waiters();
    }

    pub fn enter(&self) -> Option<InflightGuard> {
        if !self.is_ready() { return None; }
        self.inflight.fetch_add(1, Ordering::AcqRel);
        if !self.is_ready() {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(InflightGuard { lifecycle: self.clone() })
    }

    pub async fn wait_for_zero(&self) {
        while self.inflight() != 0 {
            self.changed.notified().await;
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if self.lifecycle.inflight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.lifecycle.changed.notify_waiters();
        }
    }
}
