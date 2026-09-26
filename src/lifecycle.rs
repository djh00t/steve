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

impl Phase {
    fn as_u8(self) -> u8 {
        match self {
            Self::Starting => 0,
            Self::Ready => 1,
            Self::Draining => 2,
            Self::Stopped => 3,
        }
    }
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
        let lifecycle = Self {
            phase: Arc::new(AtomicU8::new(Phase::Starting.as_u8())),
            inflight: Arc::new(AtomicU64::new(0)),
            changed: Arc::new(Notify::new()),
        };
        tracing::info!(
            event = "state_initialized",
            state = ?Phase::Starting,
            "server state initialized"
        );
        lifecycle
    }

    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Acquire) {
            1 => Phase::Ready,
            2 => Phase::Draining,
            3 => Phase::Stopped,
            _ => Phase::Starting,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.phase() == Phase::Ready
    }

    pub fn inflight(&self) -> u64 {
        self.inflight.load(Ordering::Acquire)
    }

    pub fn ready(&self, reason: &'static str) {
        self.transition(Phase::Ready, reason);
    }

    pub fn drain(&self, reason: &'static str) {
        self.transition(Phase::Draining, reason);
    }

    pub fn stopped(&self, reason: &'static str) {
        self.transition(Phase::Stopped, reason);
    }

    fn transition(&self, next: Phase, reason: &'static str) {
        let previous = self.phase();
        if previous == next {
            return;
        }

        self.phase.store(next.as_u8(), Ordering::Release);
        self.changed.notify_waiters();

        tracing::info!(
            event = "state_changed",
            from = ?previous,
            to = ?next,
            reason,
            inflight = self.inflight(),
            "server state changed"
        );
    }

    pub fn enter(&self) -> Option<InflightGuard> {
        if !self.is_ready() {
            return None;
        }

        self.inflight.fetch_add(1, Ordering::AcqRel);
        if !self.is_ready() {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return None;
        }

        Some(InflightGuard {
            lifecycle: self.clone(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drain_rejects_new_work_and_waits_for_inflight() {
        let lifecycle = Lifecycle::new();
        lifecycle.ready("test");

        let guard = lifecycle
            .enter()
            .expect("ready lifecycle should accept work");
        assert_eq!(lifecycle.inflight(), 1);

        lifecycle.drain("test");
        assert_eq!(lifecycle.phase(), Phase::Draining);
        assert!(lifecycle.enter().is_none());

        let waiter = {
            let lifecycle = lifecycle.clone();
            tokio::spawn(async move {
                lifecycle.wait_for_zero().await;
            })
        };

        assert!(!waiter.is_finished());
        drop(guard);
        waiter.await.expect("waiter should complete");
        assert_eq!(lifecycle.inflight(), 0);
    }
}
