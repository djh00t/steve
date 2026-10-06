//! Scratch-only adapter acceptance tests. Synthetic content; no provider requests.
use agent_llm::{
	CacheTokenConvention, InputFormat, LLMInfo, LLMRequest, LLMResponse, LogContentFields,
	StreamingUsageGuard, StreamingUsageReporter,
};
use http_body_util::BodyExt;
use std::sync::{
	Arc, Mutex,
	atomic::{AtomicUsize, Ordering},
};

struct Capture {
	info: Arc<Mutex<LLMInfo>>,
	reports: Arc<AtomicUsize>,
}
impl StreamingUsageReporter for Capture {
	fn update(&self, f: &mut dyn FnMut(&mut LLMInfo)) {
		f(&mut self.info.lock().unwrap());
	}
	fn report_usage(&mut self) {
		self.reports.fetch_add(1, Ordering::SeqCst);
	}
}
impl Drop for Capture {
	fn drop(&mut self) {
		self.report_usage();
	}
}

async fn convert(input: &'static str) -> (Vec<u8>, LLMResponse, usize) {
	let info = Arc::new(Mutex::new(LLMInfo::new(
		LLMRequest {
			input_tokens: None,
			input_format: InputFormat::Completions,
			cache_convention: CacheTokenConvention::InputIncludesCache,
			request_model: "public-model".into(),
			provider: "fixture-provider".into(),
			streaming: true,
			params: Default::default(),
			prompt: None,
			provider_state: None,
		},
		LLMResponse::default(),
	)));
	let reports = Arc::new(AtomicUsize::new(0));
	let body = agent_llm::conversion::completions::from_messages::translate_stream(
		agent_http::Body::from(input),
		1024 * 1024,
		StreamingUsageGuard::new(Box::new(Capture {
			info: info.clone(),
			reports: reports.clone(),
		})),
		LogContentFields::default(),
	);
	let wire = body.collect().await.unwrap().to_bytes().to_vec();
	let response = info.lock().unwrap().response.clone();
	(wire, response, reports.load(Ordering::SeqCst))
}

#[tokio::test]
async fn conversion_preserves_cache_usage_without_capturing_content() {
	// Extends the pinned completions_to_messages_stream_preserves_cache_usage fixture.
	let input = concat!(
		"data: {\"id\":\"chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"SYNTHETIC_VISIBLE_CONTENT\"}}],\"model\":\"fixture-upstream\"}\n\n",
		"data: {\"id\":\"chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"model\":\"fixture-upstream\",\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":5,\"total_tokens\":105,\"prompt_tokens_details\":{\"cached_tokens\":20,\"cache_write_tokens\":30}}}\n\n",
		"data: [DONE]\n\n"
	);
	let (wire, response, reports) = convert(input).await;
	let delta = std::str::from_utf8(&wire)
		.unwrap()
		.lines()
		.filter_map(|line| line.strip_prefix("data: "))
		.filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
		.find(|event| event["type"] == "message_delta")
		.unwrap();
	assert_eq!(delta["usage"]["input_tokens"], 50);
	assert_eq!(delta["usage"]["output_tokens"], 5);
	assert_eq!(response.input_tokens, Some(100));
	assert_eq!(response.output_tokens, Some(5));
	assert_eq!(response.cached_input_tokens, Some(20));
	assert_eq!(response.cache_creation_input_tokens, Some(30));
	assert!(response.completion.is_none());
	assert!(response.output_messages.is_none());
	assert_eq!(reports, 1);
}

#[tokio::test]
async fn absent_usage_remains_unknown_in_reporter_even_if_wire_uses_zero() {
	let (wire, response, reports) = convert(concat!(
        "data: {\"id\":\"chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"model\":\"fixture-upstream\",\"usage\":null}\n\n",
        "data: [DONE]\n\n"
    )).await;
	assert!(!wire.is_empty());
	assert_eq!(response.input_tokens, None);
	assert_eq!(response.output_tokens, None);
	assert_eq!(response.total_tokens, None);
	assert!(response.completion.is_none());
	assert_eq!(reports, 1);
}

#[test]
fn malformed_response_logging_contains_raw_marker() {
	use std::sync::atomic::AtomicBool;
	struct Intercept(Arc<AtomicBool>);
	struct MarkerVisitor<'a>(&'a AtomicBool);
	impl tracing::field::Visit for MarkerVisitor<'_> {
		fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
			if field.name() == "body" && format!("{value:?}").contains("SYNTHETIC_PARSE_MARKER") {
				self.0.store(true, Ordering::SeqCst);
			}
		}
	}
	impl tracing::Subscriber for Intercept {
		fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
			true
		}
		fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
			tracing::span::Id::from_u64(1)
		}
		fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
		fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
		fn event(&self, event: &tracing::Event<'_>) {
			event.record(&mut MarkerVisitor(&self.0));
		}
		fn enter(&self, _: &tracing::span::Id) {}
		fn exit(&self, _: &tracing::span::Id) {}
	}
	let leaked = Arc::new(AtomicBool::new(false));
	tracing::subscriber::with_default(Intercept(leaked.clone()), || {
		let body = b"SYNTHETIC_PARSE_MARKER";
		let error = serde_json::from_slice::<serde_json::Value>(body).unwrap_err();
		let _ = agent_llm::logged_response_parsing(body)(error);
	});
	// Deliberately confirms the adoption blocker, without forwarding the event to a logger.
	assert!(
		leaked.load(Ordering::SeqCst),
		"upstream malformed-response raw-body logging was not observed"
	);
}
