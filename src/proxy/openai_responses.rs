//! OpenAI Responses ingress parsing.
//!
//! `POST /v1/responses` becomes a logical [`Request`](crate::proxy::Request)
//! and a pending [`RequestAttempt`](crate::proxy::RequestAttempt). Invalid bodies
//! are HTTP 400 in the Steve error model. A valid body has no upstream in this
//! slice, so the route returns HTTP 501 with a typed stub.

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
struct ResponseInputItem {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<Value>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
enum ResponsesInput {
    Text(String),
    Items(Vec<ResponseInputItem>),
}

#[derive(Debug, Deserialize)]
struct ResponsesBody {
    model: String,
    input: ResponsesInput,
    #[serde(default)]
    stream: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct ResponsesHandoff {
    request: Request,
    attempt: RequestAttempt,
    model: String,
    input: ResponsesInput,
    stream: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct ResponsesStub {
    request_id: RequestId,
    attempt_id: AttemptId,
    model: String,
    stream: bool,
    status: &'static str,
}

impl From<&ResponsesHandoff> for ResponsesStub {
    fn from(handoff: &ResponsesHandoff) -> Self {
        Self {
            request_id: handoff.request.id,
            attempt_id: handoff.attempt.id,
            model: handoff.model.clone(),
            stream: handoff.stream,
            status: "not_implemented",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ResponsesReplyBody {
    Error(SteveErrorResponse),
    Stub(ResponsesStub),
}

pub(crate) struct ResponsesReply {
    pub(crate) status: StatusCode,
    pub(crate) body: ResponsesReplyBody,
}

pub(crate) fn handle_responses(body: &[u8]) -> ResponsesReply {
    match parse_responses(body) {
        Ok(handoff) => {
            record_handoff(&handoff);
            ResponsesReply {
                status: StatusCode::NOT_IMPLEMENTED,
                body: ResponsesReplyBody::Stub(ResponsesStub::from(&handoff)),
            }
        }
        Err(error) => ResponsesReply {
            status: StatusCode::BAD_REQUEST,
            body: ResponsesReplyBody::Error(SteveErrorResponse { error }),
        },
    }
}

fn parse_responses(body: &[u8]) -> Result<ResponsesHandoff, SteveError> {
    let parsed: ResponsesBody = serde_json::from_slice(body).map_err(invalid_json)?;
    validate_responses(&parsed)?;

    let created_at = Utc::now();
    let request_id = RequestId(Uuid::now_v7());
    let attempt_id = AttemptId(Uuid::now_v7());

    Ok(ResponsesHandoff {
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
        input: parsed.input,
        stream: parsed.stream,
    })
}

fn validate_responses(body: &ResponsesBody) -> Result<(), SteveError> {
    if body.model.trim().is_empty() {
        return Err(invalid_request(
            "model must be a non-empty string",
            Some("model".to_string()),
            "invalid_model",
        ));
    }

    match &body.input {
        ResponsesInput::Text(text) if text.trim().is_empty() => Err(invalid_request(
            "input must be a non-empty string or a non-empty item array",
            Some("input".to_string()),
            "invalid_input",
        )),
        ResponsesInput::Items(items) if items.is_empty() => Err(invalid_request(
            "input must be a non-empty string or a non-empty item array",
            Some("input".to_string()),
            "invalid_input",
        )),
        ResponsesInput::Text(_) => Ok(()),
        ResponsesInput::Items(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_input_item(index, item)?;
            }
            Ok(())
        }
    }
}

fn validate_input_item(index: usize, item: &ResponseInputItem) -> Result<(), SteveError> {
    if item
        .kind
        .as_deref()
        .is_some_and(|kind| kind.trim().is_empty())
    {
        return Err(invalid_request(
            format!("input[{index}].type must be a non-empty string"),
            Some(format!("input[{index}].type")),
            "invalid_type",
        ));
    }

    let message = item.kind.as_deref().map(str::trim).unwrap_or("message") == "message";

    if message
        && item
            .role
            .as_deref()
            .is_none_or(|role| role.trim().is_empty())
    {
        return Err(invalid_request(
            format!("input[{index}].role must be a non-empty string"),
            Some(format!("input[{index}].role")),
            "invalid_role",
        ));
    }

    if item
        .role
        .as_deref()
        .is_some_and(|role| role.trim().is_empty())
    {
        return Err(invalid_request(
            format!("input[{index}].role must be a non-empty string"),
            Some(format!("input[{index}].role")),
            "invalid_role",
        ));
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

fn record_handoff(handoff: &ResponsesHandoff) {
    let (input_kind, item_count, text_len, items_with_content) = match &handoff.input {
        ResponsesInput::Text(text) => ("text", 0, text.len(), 0),
        ResponsesInput::Items(items) => (
            "items",
            items.len(),
            0,
            items.iter().filter(|item| item.content.is_some()).count(),
        ),
    };

    tracing::info!(
        event = "responses_parsed",
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
        stream = handoff.stream,
        input_kind,
        item_count,
        text_len,
        items_with_content,
        "responses ingress parsed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error_body(reply: ResponsesReply) -> SteveErrorResponse {
        match reply.body {
            ResponsesReplyBody::Error(error) => error,
            ResponsesReplyBody::Stub(_) => panic!("expected Steve error"),
        }
    }

    fn stub_body(reply: ResponsesReply) -> ResponsesStub {
        match reply.body {
            ResponsesReplyBody::Stub(stub) => stub,
            ResponsesReplyBody::Error(_) => panic!("expected typed stub"),
        }
    }

    #[test]
    fn bad_json_is_400_steve_error() {
        let reply = handle_responses(b"{");
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
        let missing_model = handle_responses(br#"{"input":"hi"}"#);
        assert_eq!(missing_model.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_model).error.code, "invalid_body");

        let missing_input = handle_responses(br#"{"model":"gpt-test"}"#);
        assert_eq!(missing_input.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_input).error.code, "invalid_body");

        let empty_model = handle_responses(br#"{"model":"  ","input":"hi"}"#);
        assert_eq!(empty_model.status, StatusCode::BAD_REQUEST);
        let empty_model = error_body(empty_model);
        assert_eq!(empty_model.error.code, "invalid_model");
        assert_eq!(empty_model.error.param.as_deref(), Some("model"));

        let empty_input = handle_responses(br#"{"model":"gpt-test","input":"  "}"#);
        assert_eq!(empty_input.status, StatusCode::BAD_REQUEST);
        let empty_input = error_body(empty_input);
        assert_eq!(empty_input.error.code, "invalid_input");
        assert_eq!(empty_input.error.param.as_deref(), Some("input"));

        let empty_items = handle_responses(br#"{"model":"gpt-test","input":[]}"#);
        assert_eq!(empty_items.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(empty_items).error.code, "invalid_input");

        let empty_role =
            handle_responses(br#"{"model":"gpt-test","input":[{"role":"  ","content":"hi"}]}"#);
        assert_eq!(empty_role.status, StatusCode::BAD_REQUEST);
        let empty_role = error_body(empty_role);
        assert_eq!(empty_role.error.code, "invalid_role");
        assert_eq!(empty_role.error.param.as_deref(), Some("input[0].role"));

        let missing_role = handle_responses(br#"{"model":"gpt-test","input":[{"content":"hi"}]}"#);
        assert_eq!(missing_role.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(missing_role).error.code, "invalid_role");

        let empty_type =
            handle_responses(br#"{"model":"gpt-test","input":[{"type":"  ","id":"item_1"}]}"#);
        assert_eq!(empty_type.status, StatusCode::BAD_REQUEST);
        let empty_type = error_body(empty_type);
        assert_eq!(empty_type.error.code, "invalid_type");
        assert_eq!(empty_type.error.param.as_deref(), Some("input[0].type"));

        let bad_stream = handle_responses(br#"{"model":"gpt-test","input":"hi","stream":"yes"}"#);
        assert_eq!(bad_stream.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(bad_stream).error.code, "invalid_body");

        let bad_input = handle_responses(br#"{"model":"gpt-test","input":1}"#);
        assert_eq!(bad_input.status, StatusCode::BAD_REQUEST);
        assert_eq!(error_body(bad_input).error.code, "invalid_body");
    }

    #[test]
    fn valid_text_input_reaches_handler_as_request_attempt_stub() {
        let raw = br#"{"model":"gpt-test","input":"hi","temperature":0}"#;
        let handoff = parse_responses(raw).expect("valid body");

        assert_eq!(handoff.model, "gpt-test");
        assert!(!handoff.stream);
        assert_eq!(handoff.input, ResponsesInput::Text("hi".to_string()));
        assert_eq!(handoff.request.attempts, vec![handoff.attempt.id]);
        assert_eq!(handoff.attempt.request_id, handoff.request.id);
        assert_eq!(handoff.attempt.status, AttemptStatus::Pending);
        assert!(handoff.attempt.finished_at.is_none());
        assert_eq!(handoff.attempt.provider, UNASSIGNED);
        assert_eq!(handoff.attempt.account, UNASSIGNED);
        assert_eq!(handoff.request.created_at, handoff.attempt.started_at);

        let stub = ResponsesStub::from(&handoff);
        assert_eq!(stub.request_id, handoff.request.id);
        assert_eq!(stub.attempt_id, handoff.attempt.id);
        assert_eq!(stub.model, handoff.model);
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");

        let reply = handle_responses(raw);
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        let stub = stub_body(reply);
        assert_eq!(stub.model, "gpt-test");
        assert!(!stub.stream);
        assert_eq!(stub.status, "not_implemented");
        assert_ne!(stub.request_id.0, Uuid::nil());
        assert_ne!(stub.attempt_id.0, Uuid::nil());
    }

    #[test]
    fn valid_item_array_keeps_message_content() {
        let raw = br#"{"model":"gpt-test","input":[{"role":"user","content":"hi"},{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]},{"type":"item_reference","id":"msg_1"}]}"#;
        let handoff = parse_responses(raw).expect("valid items");

        let ResponsesInput::Items(items) = handoff.input else {
            panic!("expected item array");
        };
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].role.as_deref(), Some("user"));
        assert_eq!(items[0].content, Some(json!("hi")));
        assert!(items[0].kind.is_none());
        assert_eq!(items[1].kind.as_deref(), Some("message"));
        assert_eq!(items[1].role.as_deref(), Some("assistant"));
        assert_eq!(
            items[1].content,
            Some(json!([{"type": "output_text", "text": "ok"}]))
        );
        assert_eq!(items[2].kind.as_deref(), Some("item_reference"));
        assert!(items[2].role.is_none());
        assert!(items[2].content.is_none());
    }

    #[test]
    fn stream_flag_defaults_false_and_can_be_true() {
        let omitted = parse_responses(br#"{"model":"gpt-test","input":"hi"}"#).expect("omitted");
        assert!(!omitted.stream);

        let enabled =
            parse_responses(br#"{"model":"gpt-test","input":[{"role":"user"}],"stream":true}"#)
                .expect("stream true");
        assert!(enabled.stream);

        let reply = handle_responses(br#"{"model":"gpt-test","input":"hi","stream":true}"#);
        assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
        assert!(stub_body(reply).stream);
    }
}
