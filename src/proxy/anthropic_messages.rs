//! Anthropic Messages ingress parsing.
//!
//! `POST /v1/messages` on the inference listener. Clients send:
//!
//! - `content-type: application/json`
//! - `x-api-key: <api-key>`
//! - `anthropic-version: 2023-06-01`
//!
//! A minimal body is `model`, `max_tokens`, and `messages`, with optional
//! `stream` (default `false`). Invalid bodies are HTTP 400 in the Steve error
//! model. A valid body has no upstream in this slice, so the route returns
//! HTTP 501 with a typed stub.

use super::{AttemptId, AttemptStatus, Request, RequestAttempt, RequestId};
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const UNASSIGNED: &str = "unassigned";
const INVALID_REQUEST: &str = "invalid_request_error";

#[derive(Clone, Debug, PartialEq, Serialize)]
struct SteveError {
    message: String,
    #[serde(rename = "type")]
    kind: &'static str,
    code: &'static str,
    param: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct SteveErrorResponse {
    error: SteveError,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct Message {
    role: String,
    #[serde(default)]
    content: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct MessagesBody {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
    #[serde(default)]
    stream: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct MessagesHandoff {
    request: Request,
    attempt: RequestAttempt,
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
    stream: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct MessagesStub {
    request_id: RequestId,
    attempt_id: AttemptId,
    model: String,
    max_tokens: u32,
    stream: bool,
    status: &'static str,
}

impl From<&MessagesHandoff> for MessagesStub {
    fn from(handoff: &MessagesHandoff) -> Self {
        Self {
            request_id: handoff.request.id,
            attempt_id: handoff.attempt.id,
            model: handoff.model.clone(),
            max_tokens: handoff.max_tokens,
            stream: handoff.stream,
            status: "not_implemented",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum MessagesReplyBody {
    Error(SteveErrorResponse),
    Stub(MessagesStub),
}

pub(crate) struct MessagesReply {
    pub(crate) status: StatusCode,
    pub(crate) body: MessagesReplyBody,
}

pub(crate) fn handle_messages(body: &[u8]) -> MessagesReply {
    match parse_messages(body) {
        Ok(handoff) => {
            record_handoff(&handoff);
            MessagesReply {
                status: StatusCode::NOT_IMPLEMENTED,
                body: MessagesReplyBody::Stub(MessagesStub::from(&handoff)),
            }
        }
        Err(error) => MessagesReply {
            status: StatusCode::BAD_REQUEST,
            body: MessagesReplyBody::Error(SteveErrorResponse { error }),
        },
    }
}

fn parse_messages(body: &[u8]) -> Result<MessagesHandoff, SteveError> {
    let parsed: MessagesBody = serde_json::from_slice(body).map_err(invalid_json)?;
    validate_messages(&parsed)?;

    let created_at = Utc::now();
    let request_id = RequestId(Uuid::now_v7());
    let attempt_id = AttemptId(Uuid::now_v7());

    Ok(MessagesHandoff {
        request: Request {
            id: request_id,
            created_at,
            attempts: vec![attempt_id],
        },
        attempt: RequestAttempt {
            id: attempt_id,
            request_id,
            provider: UNASSIGNED.to_string(),
            account: UNASSIGNED.to_string(),
            status: AttemptStatus::Pending,
            started_at: created_at,
            finished_at: None,
        },
        model: parsed.model,
        max_tokens: parsed.max_tokens,
        messages: parsed.messages,
        stream: parsed.stream,
    })
}

fn validate_messages(body: &MessagesBody) -> Result<(), SteveError> {
    if body.model.trim().is_empty() {
        return Err(invalid_request(
            "model must be a non-empty string",
            Some("model".to_string()),
            "invalid_model",
        ));
    }

    if body.max_tokens == 0 {
        return Err(invalid_request(
            "max_tokens must be a positive integer",
            Some("max_tokens".to_string()),
            "invalid_max_tokens",
        ));
    }

    if body.messages.is_empty() {
        return Err(invalid_request(
            "messages must contain at least one message",
            Some("messages".to_string()),
            "invalid_messages",
        ));
    }

    for (index, message) in body.messages.iter().enumerate() {
        if message.role.trim().is_empty() {
            return Err(invalid_request(
                format!("messages[{index}].role must be a non-empty string"),
                Some(format!("messages[{index}].role")),
                "invalid_role",
            ));
        }
    }

    Ok(())
}

fn invalid_json(err: serde_json::Error) -> SteveError {
    let code = if err.is_syntax() || err.is_eof() {
        "invalid_json"
    } else {
        "invalid_body"
    };
    invalid_request(err.to_string(), None, code)
}

fn invalid_request(
    message: impl Into<String>,
    param: Option<String>,
    code: &'static str,
) -> SteveError {
    SteveError {
        message: message.into(),
        kind: INVALID_REQUEST,
        code,
        param,
    }
}

fn record_handoff(handoff: &MessagesHandoff) {
    let messages_with_content = handoff
        .messages
        .iter()
        .filter(|message| message.content.is_some())
        .count();

    tracing::info!(
        event = "messages_parsed",
        request_id = %handoff.request.id.0,
        created_at = %handoff.request.created_at,
        attempt_count = handoff.request.attempts.len(),
        attempt_id = %handoff.attempt.id.0,
        attempt_request_id = %handoff.attempt.request_id.0,
        provider = %handoff.attempt.provider,
        account = %handoff.attempt.account,
        status = ?handoff.attempt.status,
        started_at = %handoff.attempt.started_at,
        finished = handoff.attempt.finished_at.is_some(),
        model = %handoff.model,
        max_tokens = handoff.max_tokens,
        stream = handoff.stream,
        message_count = handoff.messages.len(),
        messages_with_content,
        "anthropic messages ingress parsed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error_body(reply: MessagesReply) -> SteveErrorResponse {
        match reply.body {
            MessagesReplyBody::Error(error) => error,
            MessagesReplyBody::Stub(_) => panic!("expected Steve error"),
        }
    }

    fn stub_body(reply: MessagesReply) -> MessagesStub {
        match reply.body {
            MessagesReplyBody::Stub(stub) => stub,
            MessagesReplyBody::Error(_) => panic!("expected typed stub"),
        }
    }

    #[test]
    fn bad_json_is_400_steve_error() {
        let reply = handle_messages(b"{");
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);

        let error = error_body(reply);
        assert_eq!(error.error.kind, "invalid_request_error");
        assert_eq!(error.error.code, "invalid_json");
        assert!(error.error.param.is_none());

        let value = serde_json::to_value(&error).expect("serialize error");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(value["error"]["code"], "invalid_json");
        assert!(value["error"]["param"].is_null());
        assert!(!value["error"]["message"].as_str().unwrap().is_empty());
    }

    #[test]
    fn missing_and_empty_fields_are_400() {
        let missing_model =
            handle_messages(br#"{"max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#);
        assert_eq!(missing_model.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_model).error.code, "invalid_body");

        let empty_model = handle_messages(
            br#"{"model":"  ","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(empty_model.status, StatusCode::BAD_REQUEST);
        let empty_model = error_body(empty_model);
        assert_eq!(empty_model.error.code, "invalid_model");
        assert_eq!(empty_model.error.param.as_deref(), Some("model"));

        let missing_max_tokens = handle_messages(
            br#"{"model":"claude-test","messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(missing_max_tokens.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_max_tokens).error.code, "invalid_body");

        let zero_max_tokens = handle_messages(
            br#"{"model":"claude-test","max_tokens":0,"messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(zero_max_tokens.status, StatusCode::BAD_REQUEST);
        let zero_max_tokens = error_body(zero_max_tokens);
        assert_eq!(zero_max_tokens.error.code, "invalid_max_tokens");
        assert_eq!(zero_max_tokens.error.param.as_deref(), Some("max_tokens"));

        let negative_max_tokens = handle_messages(
            br#"{"model":"claude-test","max_tokens":-1,"messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(negative_max_tokens.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(negative_max_tokens).error.code, "invalid_body");

        let empty_messages =
            handle_messages(br#"{"model":"claude-test","max_tokens":16,"messages":[]}"#);
        assert_eq!(empty_messages.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(empty_messages).error.code, "invalid_messages");

        let empty_role = handle_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"  ","content":"hi"}]}"#,
        );
        assert_eq!(empty_role.status, StatusCode::BAD_REQUEST);
        let empty_role = error_body(empty_role);
        assert_eq!(empty_role.error.code, "invalid_role");
        assert_eq!(empty_role.error.param.as_deref(), Some("messages[0].role"));

        let bad_stream = handle_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"}],"stream":"yes"}"#,
        );
        assert_eq!(bad_stream.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(bad_stream).error.code, "invalid_body");
    }

    #[test]
    fn valid_body_reaches_handler_as_request_attempt_stub() {
        let raw = br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":[{"type":"text","text":"ok"}]}],"system":"be brief"}"#;
        let handoff = parse_messages(raw).expect("valid body");

        assert_eq!(handoff.model, "claude-test");
        assert_eq!(handoff.max_tokens, 16);
        assert!(!handoff.stream);
        assert_eq!(handoff.messages.len(), 2);
        assert_eq!(handoff.messages[0].role, "user");
        assert_eq!(handoff.messages[0].content, Some(json!("hi")));
        assert_eq!(
            handoff.messages[1].content,
            Some(json!([{"type": "text", "text": "ok"}]))
        );
        assert_eq!(handoff.request.attempts, vec![handoff.attempt.id]);
        assert_eq!(handoff.attempt.request_id, handoff.request.id);
        assert_eq!(handoff.attempt.status, AttemptStatus::Pending);
        assert!(handoff.attempt.finished_at.is_none());
        assert_eq!(handoff.attempt.provider, UNASSIGNED);
        assert_eq!(handoff.attempt.account, UNASSIGNED);
        assert_eq!(handoff.request.created_at, handoff.attempt.started_at);

        let stub = MessagesStub::from(&handoff);
        assert_eq!(stub.request_id, handoff.request.id);
        assert_eq!(stub.attempt_id, handoff.attempt.id);
        assert_eq!(stub.model, handoff.model);
        assert_eq!(stub.max_tokens, 16);
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");

        let reply = handle_messages(raw);
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        let stub = stub_body(reply);
        assert_eq!(stub.model, "claude-test");
        assert_eq!(stub.max_tokens, 16);
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");
        assert_ne!(stub.request_id.0, Uuid::nil());
        assert_ne!(stub.attempt_id.0, Uuid::nil());
    }

    #[test]
    fn stream_flag_defaults_false_and_can_be_true() {
        let omitted = parse_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .expect("stream omitted");
        assert!(!omitted.stream);

        let enabled = parse_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"assistant"}],"stream":true}"#,
        )
        .expect("stream true");
        assert!(enabled.stream);
        assert!(enabled.messages[0].content.is_none());

        let reply = handle_messages(
            br#"{"model":"claude-test","max_tokens":16,"messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        );
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        let stub = stub_body(reply);
        assert!(stub.stream);
        assert_eq!(stub.max_tokens, 16);
    }
}
