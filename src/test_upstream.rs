use crate::{net::bind_listener, server::shutdown_signal};
use anyhow::Result;
use axum::{routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use std::net::SocketAddr;

pub async fn run(listen: SocketAddr) -> Result<()> {
    let app = Router::new()
        .route("/health/live", get(|| async { "ok" }))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/v1/messages", post(anthropic_messages));

    let listener = bind_listener(listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(event = "test_upstream_ready", %local_addr, "deterministic test upstream ready");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_signal().await;
        })
        .await?;

    Ok(())
}

async fn chat_completions(Json(request): Json<Value>) -> Json<Value> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("steve-test-model");

    Json(json!({
        "id": "chatcmpl-steve-test",
        "object": "chat.completion",
        "created": 0,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "steve-test-response"
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 3,
            "total_tokens": 13
        }
    }))
}

async fn responses(Json(request): Json<Value>) -> Json<Value> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("steve-test-model");

    Json(json!({
        "id": "resp_steve_test",
        "object": "response",
        "status": "completed",
        "model": model,
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": "steve-test-response"
            }]
        }],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3,
            "total_tokens": 13
        }
    }))
}

async fn anthropic_messages(Json(request): Json<Value>) -> Json<Value> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("steve-test-model");

    Json(json!({
        "id": "msg_steve_test",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{
            "type": "text",
            "text": "steve-test-response"
        }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3
        }
    }))
}
