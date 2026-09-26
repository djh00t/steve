//! Upstream byte/SSE pump, client-disconnect cancellation, and the no-replay rule.
//!
//! Chat Completions, Responses, and Messages ingress reuse this module to
//! forward an upstream body, abort that upstream when the client disconnects,
//! and refuse a silent second attempt after the first output byte.

use axum::body::Body;
use bytes::Bytes;
use std::{
    error::Error,
    future::poll_fn,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
};
use thiserror::Error as ThisError;
use tokio::sync::{mpsc, watch};
use tokio_stream::{wrappers::ReceiverStream, Stream};

/// `Content-Type` for an SSE response body produced by [`pump_upstream`].
pub const SSE_CONTENT_TYPE: &str = "text/event-stream";

const PUMP_BUFFER: usize = 16;

/// Cooperative cancel signal. Dropping the pumped response body cancels it;
/// upstream work should select on [`CancelToken::cancelled`] and stop.
#[derive(Clone, Debug)]
pub struct CancelToken {
    tx: Arc<watch::Sender<bool>>,
    rx: watch::Receiver<bool>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }

    pub fn cancel(&self) {
        self.tx.send_if_modified(|cancelled| {
            if *cancelled {
                return false;
            }
            *cancelled = true;
            true
        });
    }

    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Resolves when [`CancelToken::cancel`] has been called.
    pub async fn cancelled(&self) {
        self.cancelled_future().await;
    }

    /// `'static` cancel wait for streams that outlive this borrow.
    ///
    /// The upstream SSE client polls this while reading the response body so
    /// cancellation aborts the read without buffering the rest of the stream.
    /// Dropping the future does not cancel the token.
    #[must_use = "the cancel wait does nothing unless polled"]
    pub fn cancelled_future(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut rx = self.rx.clone();
        async move {
            loop {
                if *rx.borrow_and_update() {
                    return;
                }
                if rx.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Per-logical-request gate for the no-silent-replay rule.
///
/// [`ReplayGate::mark_output_begun`] flips after the pump commits the first
/// non-empty chunk. Later attempts must stop before opening a new upstream.
#[derive(Clone, Debug, Default)]
pub struct ReplayGate {
    output_begun: Arc<AtomicBool>,
}

impl ReplayGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn output_begun(&self) -> bool {
        self.output_begun.load(Ordering::Acquire)
    }

    pub fn mark_output_begun(&self) {
        if !self.output_begun.swap(true, Ordering::AcqRel) {
            tracing::debug!(
                event = "stream_output_begun",
                "first output byte committed; silent replay forbidden"
            );
        }
    }

    /// `Err` once any output byte has been committed to the client body.
    pub fn ensure_can_attempt(&self) -> Result<(), ReplayForbidden> {
        if self.output_begun() {
            tracing::debug!(
                event = "stream_replay_forbidden",
                "refusing silent replay after output began"
            );
            Err(ReplayForbidden)
        } else {
            Ok(())
        }
    }
}

/// Automatic retry/failover is not allowed after streamed output has begun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ThisError)]
#[error("silent replay is forbidden after output has begun")]
pub struct ReplayForbidden;

/// One SSE `data:` frame, including the trailing blank line.
pub fn sse_data(payload: &str) -> Bytes {
    Bytes::from(format!("data: {payload}\n\n"))
}

/// OpenAI-style terminal SSE sentinel.
pub fn sse_done() -> Bytes {
    Bytes::from_static(b"data: [DONE]\n\n")
}

/// Forward `upstream` to an axum [`Body`].
///
/// * The first non-empty chunk sets [`ReplayGate::output_begun`]. Empty
///   chunks (SSE comments, keepalives with no payload) do not.
/// * Dropping the body, or an explicit [`CancelToken::cancel`], aborts the
///   pump and drops `upstream`.
/// * A second call after output has begun returns [`ReplayForbidden`] and
///   does not poll `upstream`.
pub fn pump_upstream<S, E>(
    gate: &ReplayGate,
    cancel: CancelToken,
    upstream: S,
) -> Result<Body, ReplayForbidden>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Error + Send + Sync + 'static,
{
    gate.ensure_can_attempt()?;

    let (tx, rx) = mpsc::channel(PUMP_BUFFER);
    let task_cancel = cancel.clone();
    let task_gate = gate.clone();

    tokio::spawn(async move {
        let mut upstream = std::pin::pin!(upstream);
        loop {
            tokio::select! {
                biased;
                () = task_cancel.cancelled() => {
                    tracing::debug!(
                        event = "stream_client_disconnect",
                        "client disconnected; aborting upstream"
                    );
                    break;
                }
                item = poll_fn(|cx| upstream.as_mut().poll_next(cx)) => {
                    match item {
                        Some(Ok(chunk)) => {
                            let nonempty = !chunk.is_empty();
                            if tx.send(Ok(chunk)).await.is_err() {
                                task_cancel.cancel();
                                break;
                            }
                            if nonempty {
                                task_gate.mark_output_begun();
                            }
                        }
                        Some(Err(err)) => {
                            let _ = tx.send(Err(std::io::Error::other(err))).await;
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
    });

    Ok(Body::from_stream(DisconnectStream {
        inner: ReceiverStream::new(rx),
        cancel,
    }))
}

/// Cancels `cancel` when the client drops the response body.
struct DisconnectStream {
    inner: ReceiverStream<Result<Bytes, std::io::Error>>,
    cancel: CancelToken,
}

impl Stream for DisconnectStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().inner).poll_next(cx)
    }
}

impl Drop for DisconnectStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct IdleUpstream {
        aborted: Arc<AtomicBool>,
    }

    impl Drop for IdleUpstream {
        fn drop(&mut self) {
            self.aborted.store(true, Ordering::SeqCst);
        }
    }

    impl Stream for IdleUpstream {
        type Item = Result<Bytes, std::io::Error>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Pending
        }
    }

    struct DropSignal<S> {
        inner: S,
        dropped: Arc<AtomicBool>,
    }

    impl<S> Drop for DropSignal<S> {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl<S: Stream + Unpin> Stream for DropSignal<S> {
        type Item = S::Item;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Pin::new(&mut self.get_mut().inner).poll_next(cx)
        }
    }

    struct UnpolledUpstream;

    impl Stream for UnpolledUpstream {
        type Item = Result<Bytes, std::io::Error>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            panic!("replay must not poll a new upstream");
        }
    }

    async fn until(mut ready: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async move {
            while !ready() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for stream pump");
    }

    #[tokio::test]
    async fn disconnect_aborts_upstream() {
        let cancel = CancelToken::new();
        let gate = ReplayGate::new();
        let aborted = Arc::new(AtomicBool::new(false));
        let upstream = IdleUpstream {
            aborted: aborted.clone(),
        };

        let body = pump_upstream(&gate, cancel.clone(), upstream).expect("pump should start");
        drop(body);

        until(|| aborted.load(Ordering::SeqCst) && cancel.is_cancelled()).await;
        assert!(
            !gate.output_begun(),
            "disconnect before any byte must not begin output"
        );
        assert!(gate.ensure_can_attempt().is_ok());
    }

    #[tokio::test]
    async fn no_replay_after_output_begun() {
        let gate = ReplayGate::new();
        assert_eq!(SSE_CONTENT_TYPE, "text/event-stream");
        assert!(gate.ensure_can_attempt().is_ok());

        let dropped = Arc::new(AtomicBool::new(false));
        let empty = DropSignal {
            inner: tokio_stream::iter([Ok::<Bytes, std::io::Error>(Bytes::new())]),
            dropped: dropped.clone(),
        };
        let empty_body = pump_upstream(&gate, CancelToken::new(), empty).expect("empty pump");
        until(|| dropped.load(Ordering::SeqCst)).await;
        drop(empty_body);
        assert!(!gate.output_begun());
        assert!(gate.ensure_can_attempt().is_ok());

        let cancel = CancelToken::new();
        let body = pump_upstream(
            &gate,
            cancel,
            tokio_stream::iter([Ok::<Bytes, std::io::Error>(sse_data("hi")), Ok(sse_done())]),
        )
        .expect("first byte pump");
        until(|| gate.output_begun()).await;
        assert!(gate.ensure_can_attempt().is_err());
        drop(body);

        let forbidden = pump_upstream(&gate, CancelToken::new(), UnpolledUpstream);
        assert_eq!(forbidden.err(), Some(ReplayForbidden));
    }

    #[tokio::test]
    async fn cancelled_future_waits_until_cancel() {
        let token = CancelToken::new();
        let wait = token.cancelled_future();
        let clone = token.clone();
        let handle = tokio::spawn(async move {
            wait.await;
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !handle.is_finished(),
            "cancel wait must not complete before cancel"
        );
        clone.cancel();
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("cancel wait was not woken")
            .expect("cancel wait task");
        assert!(token.is_cancelled());
    }
}
