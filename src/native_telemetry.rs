use axum::{
    body::{Body, HttpBody},
    response::Response,
};
use http_body::{Frame, SizeHint};
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

pub(crate) struct TelemetryContext {
    pub request_id: String,
    pub public_model: String,
    pub provider: String,
    pub protocol: &'static str,
    pub started: Instant,
}

const RETAIN_LIMIT: usize = 64 * 1024;
#[derive(Default, Debug, PartialEq)]
struct Usage {
    input: Option<u64>,
    output: Option<u64>,
}
struct UsageParser {
    sse: bool,
    retained: Vec<u8>,
    overflow: bool,
    tail: [u8; 4],
    usage: Usage,
}
impl UsageParser {
    fn new(sse: bool) -> Self {
        Self {
            sse,
            retained: Vec::new(),
            overflow: false,
            tail: [0; 4],
            usage: Usage::default(),
        }
    }
    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.tail.rotate_left(1);
            self.tail[3] = byte;
            if !self.overflow {
                if self.retained.len() == RETAIN_LIMIT {
                    self.retained.clear();
                    self.overflow = true;
                } else {
                    self.retained.push(byte);
                }
            }
            if self.sse && (self.tail.ends_with(b"\n\n") || self.tail == *b"\r\n\r\n") {
                self.finish();
                self.retained.clear();
                self.overflow = false;
                self.tail = [0; 4];
            }
        }
    }
    fn finish(&mut self) {
        if self.overflow {
            return;
        }
        let payload = if self.sse {
            self.retained
                .split(|&b| b == b'\n')
                .filter_map(|line| {
                    line.strip_suffix(b"\r")
                        .unwrap_or(line)
                        .strip_prefix(b"data:")
                })
                .map(|line| line.strip_prefix(b" ").unwrap_or(line))
                .collect::<Vec<_>>()
                .join(&b'\n')
        } else {
            self.retained.clone()
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else {
            return;
        };
        let usage = value
            .get("usage")
            .or_else(|| value.get("response").and_then(|v| v.get("usage")))
            .or_else(|| value.get("message").and_then(|v| v.get("usage")));
        if let Some(usage) = usage {
            if let Some(input) = usage
                .get("input_tokens")
                .or_else(|| usage.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
            {
                self.usage.input = Some(input);
            }
            if let Some(output) = usage
                .get("output_tokens")
                .or_else(|| usage.get("completion_tokens"))
                .and_then(|v| v.as_u64())
            {
                self.usage.output = Some(output);
            }
        }
    }
}

pub(crate) fn observe(response: Response, context: TelemetryContext) -> Response {
    let (parts, inner) = response.into_parts();
    let sse = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("text/event-stream")
        });
    let mut body = ObservedBody {
        inner,
        context,
        status: parts.status.as_u16(),
        parser: UsageParser::new(sse),
        bytes: 0,
        first_byte_ms: None,
        logged: false,
    };
    if body.inner.is_end_stream() {
        body.finish(true, false);
    }
    Response::from_parts(parts, Body::new(body))
}

struct ObservedBody {
    inner: Body,
    context: TelemetryContext,
    status: u16,
    parser: UsageParser,
    bytes: u64,
    first_byte_ms: Option<u128>,
    logged: bool,
}
impl ObservedBody {
    fn finish(&mut self, completed: bool, error: bool) {
        if self.logged {
            return;
        }
        self.logged = true;
        if completed {
            self.parser.finish();
        }
        tracing::info!(
            request_id = %self.context.request_id,
            public_model = %self.context.public_model,
            provider = %self.context.provider,
            protocol = self.context.protocol,
            status = self.status,
            completed, cancelled = !completed && !error, error,
            latency_ms = self.context.started.elapsed().as_millis(),
            first_byte_ms = ?self.first_byte_ms,
            bytes = self.bytes,
            input_tokens = ?self.parser.usage.input,
            output_tokens = ?self.parser.usage.output,
            "native_response"
        );
    }
}
impl HttpBody for ObservedBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    if !data.is_empty() && this.first_byte_ms.is_none() {
                        this.first_byte_ms = Some(this.context.started.elapsed().as_millis());
                    }
                    this.bytes = this.bytes.saturating_add(data.len() as u64);
                    this.parser.feed(data);
                }
                if this.inner.is_end_stream() {
                    this.finish(true, false);
                }
            }
            Poll::Ready(None) => this.finish(true, false),
            Poll::Ready(Some(Err(_))) => this.finish(false, true),
            Poll::Pending => {}
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
impl Drop for ObservedBody {
    fn drop(&mut self) {
        self.finish(false, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> TelemetryContext {
        TelemetryContext {
            request_id: "req".into(),
            public_model: "public".into(),
            provider: "provider".into(),
            protocol: "chat",
            started: Instant::now(),
        }
    }
    #[derive(Clone)]
    struct LogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn passes_raw_frames_and_logs_completion_error_and_drop_once() {
        use http_body_util::BodyExt;
        let logs = LogWriter(Default::default());
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let frames = vec![
            Ok::<_, std::io::Error>(Frame::data(bytes::Bytes::from_static(
                b"raw secret payload",
            ))),
            Ok(Frame::trailers(axum::http::HeaderMap::new())),
        ];
        let mut response = observe(
            Response::new(Body::new(http_body_util::StreamBody::new(
                tokio_stream::iter(frames),
            ))),
            context(),
        );
        let first = response.body_mut().frame().await.unwrap().unwrap();
        assert_eq!(first.data_ref().unwrap(), b"raw secret payload".as_slice());
        assert!(response
            .body_mut()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .is_trailers());
        assert!(response.body_mut().frame().await.is_none());
        drop(response);
        let errors = tokio_stream::iter([Err::<Frame<bytes::Bytes>, _>(std::io::Error::other(
            "secret error",
        ))]);
        let mut response = observe(
            Response::new(Body::new(http_body_util::StreamBody::new(errors))),
            context(),
        );
        assert!(response.body_mut().frame().await.unwrap().is_err());
        drop(response);
        drop(observe(
            Response::new(Body::from("never consumed")),
            context(),
        ));
        let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert_eq!(text.matches("native_response").count(), 3, "{text}");
        assert!(text.contains("completed=true"), "{text}");
        assert!(text.contains("cancelled=true"), "{text}");
        assert!(text.contains("error=true"), "{text}");
        assert!(!text.contains("secret"));
    }
    #[test]
    fn extracts_usage_across_arbitrary_chunks_and_bounds_retention() {
        for payload in [
            r#"{"usage":{"prompt_tokens":12,"completion_tokens":7}}"#,
            r#"{"response":{"usage":{"input_tokens":12,"output_tokens":7}}}"#,
        ] {
            let mut parser = UsageParser::new(false);
            for chunk in payload.as_bytes().chunks(3) {
                parser.feed(chunk);
            }
            parser.finish();
            assert_eq!(
                parser.usage,
                Usage {
                    input: Some(12),
                    output: Some(7)
                }
            );
        }
        let mut parser = UsageParser::new(true);
        let stream = concat!(
            "event: message_start\r\ndata: {\"message\":{\"usage\":{\"input_tokens\":12}}}\r\n\r\n",
            "event: message_delta\ndata: {\"usage\":{\"output_tokens\":7}}\n\n",
        );
        for byte in stream.as_bytes() {
            parser.feed(&[*byte]);
        }
        parser.finish();
        assert_eq!(
            parser.usage,
            Usage {
                input: Some(12),
                output: Some(7)
            }
        );
        parser.feed(&vec![b'x'; RETAIN_LIMIT * 3]);
        assert!(parser.retained.len() <= RETAIN_LIMIT);
        parser.feed(b"\n\ndata: {\"usage\":{\"prompt_tokens\":15}}\n\n");
        assert_eq!(parser.usage.input, Some(15));
        let mut unknown = UsageParser::new(false);
        unknown.feed(b"{\"text\":\"no usage\"}");
        unknown.finish();
        assert_eq!(unknown.usage, Usage::default());
    }
}
