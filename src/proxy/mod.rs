pub(crate) mod anthropic_messages;
#[cfg_attr(not(test), allow(dead_code))]
mod anthropic_upstream;
pub(crate) mod openai_chat;
pub(crate) mod openai_responses;
#[cfg_attr(not(test), allow(dead_code))]
mod openai_upstream;
mod types;

#[cfg_attr(not(test), allow(unused_imports))]
pub use anthropic_upstream::{AnthropicError, AnthropicEventStream, AnthropicUpstream};
#[cfg_attr(not(test), allow(unused_imports))]
pub use openai_upstream::{OpenAiUpstream, SteveError};
pub use types::{AttemptId, AttemptStatus, Request, RequestAttempt, RequestId};

// Stream pump and cancel helpers. Ingress routes call these once they land.
#[cfg_attr(not(test), allow(dead_code))]
pub mod stream;

pub(crate) fn validate_upstream_url(raw: &str, url: &reqwest::Url) -> Result<(), &'static str> {
    let target_host = url.host_str().ok_or("base URL is missing a host")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("base URL must not contain credentials");
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" => {
            let original_uri =
                axum::http::Uri::try_from(raw).map_err(|_| "http base URL authority is invalid")?;
            let original_host = original_uri.host().ok_or("base URL is missing a host")?;
            let original_host = original_host
                .strip_prefix('[')
                .and_then(|host| host.strip_suffix(']'))
                .unwrap_or(original_host);
            let original_ip = original_host
                .parse::<std::net::IpAddr>()
                .map_err(|_| "http base URL must use a numeric loopback IP")?;
            let target_host = target_host
                .strip_prefix('[')
                .and_then(|host| host.strip_suffix(']'))
                .unwrap_or(target_host);
            let target_ip = target_host
                .parse::<std::net::IpAddr>()
                .map_err(|_| "http base URL must use a numeric loopback IP")?;
            if target_ip == original_ip && target_ip.is_loopback() {
                Ok(())
            } else {
                Err("http base URL must use a loopback IP")
            }
        }
        _ => Err("base URL must use https or numeric loopback http"),
    }
}

/// Response cache metadata only; Connection can nominate even allowlisted fields.
fn safe_cache_headers(source: &reqwest::header::HeaderMap) -> axum::http::HeaderMap {
    let mut headers = axum::http::HeaderMap::new();
    for name in ["cache-control", "pragma", "expires", "vary", "date", "age"] {
        let connection_nominated = source.get_all("connection").iter().any(|value| {
            value.to_str().map_or(true, |value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case(name))
            })
        });
        if !connection_nominated {
            for value in source.get_all(name) {
                headers.append(name, value.clone());
            }
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::{AnthropicUpstream, OpenAiUpstream};
    use std::time::Duration;

    #[test]
    fn http_requires_numeric_loopback() {
        let timeout = Duration::from_secs(1);
        for base_url in [
            "https://example.com",
            "https://example.com/v1",
            "http://127.0.0.1",
            "http://127.0.0.42/v1",
            "http://[::1]",
            "http://[::1]:18080/v1",
        ] {
            assert!(
                OpenAiUpstream::new(base_url, timeout).is_ok(),
                "OpenAI should accept {base_url}"
            );
            assert!(
                AnthropicUpstream::new(base_url, timeout).is_ok(),
                "Anthropic should accept {base_url}"
            );
        }

        for base_url in [
            "http://localhost",
            "http://user:pass@127.0.0.1/v1",
            "https://user:pass@example.com/v1",
            "http://127.0.0.1.example",
            "http://2130706433",
            "http://0x7f000001",
            "http://127.1",
            "http://127%2e0%2e0%2e1",
            "http://%31%32%37.0.0.1",
            "http://192.0.2.1",
            "http://[2001:db8::1]",
        ] {
            assert!(
                OpenAiUpstream::new(base_url, timeout).is_err(),
                "OpenAI should reject {base_url}"
            );
            assert!(
                AnthropicUpstream::new(base_url, timeout).is_err(),
                "Anthropic should reject {base_url}"
            );
        }
    }
}

#[cfg(test)]
mod cache_directive_tests {
    use super::{
        anthropic_messages, openai_chat, openai_responses, AnthropicUpstream, OpenAiUpstream,
    };
    use axum::{
        body::Body,
        http::{HeaderMap, HeaderValue, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Router,
    };
    use http_body_util::BodyExt;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn cache_header_allowlist_excludes_connection_nominations() {
        let mut source = HeaderMap::new();
        for name in [
            "cache-control",
            "pragma",
            "expires",
            "vary",
            "date",
            "age",
            "set-cookie",
            "x-custom",
            "authorization",
            "content-length",
            "transfer-encoding",
        ] {
            source.append(name, HeaderValue::from_static("one"));
            source.append(name, HeaderValue::from_static("two"));
        }
        source.append(
            "connection",
            HeaderValue::from_static("keep-alive, CaChe-CoNtRoL"),
        );
        source.append("connection", HeaderValue::from_static("AGE, expires"));
        let headers = super::safe_cache_headers(&source);
        assert_eq!(headers.len(), 6);
        for name in ["pragma", "vary", "date"] {
            assert_eq!(headers.get_all(name).iter().count(), 2);
        }
        for name in [
            "cache-control",
            "age",
            "expires",
            "set-cookie",
            "x-custom",
            "authorization",
            "content-length",
            "transfer-encoding",
            "connection",
        ] {
            assert!(
                !headers.contains_key(name),
                "unsafe header {name} forwarded"
            );
        }
        source.append("connection", HeaderValue::from_bytes(b"\xff").unwrap());
        assert!(super::safe_cache_headers(&source).is_empty());
        assert!(super::safe_cache_headers(&HeaderMap::new()).is_empty());
    }

    #[tokio::test]
    async fn cache_metadata_is_request_local_and_cleared_before_failure() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(
                |axum::Json(request): axum::Json<serde_json::Value>| async move {
                    let policy = if request["model"] == "slow" {
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        "no-store"
                    } else {
                        "private"
                    };
                    let mut headers = HeaderMap::new();
                    headers.insert("cache-control", HeaderValue::from_static(policy));
                    fixture(headers, false, StatusCode::OK, false).await
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let upstream =
            OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
        let mut slow_headers = HeaderMap::new();
        let mut fast_headers = HeaderMap::new();
        let slow = json!({"model":"slow"});
        let fast = json!({"model":"fast"});
        let (slow_result, fast_result) = tokio::join!(
            upstream.chat_completion_with_headers(&slow, &mut slow_headers),
            upstream.chat_completion_with_headers(&fast, &mut fast_headers)
        );
        assert!(slow_result.is_ok() && fast_result.is_ok());
        assert_eq!(slow_headers["cache-control"], "no-store");
        assert_eq!(fast_headers["cache-control"], "private");
        let error = upstream
            .chat_completion_with_headers(&json!({"stream":true}), &mut slow_headers)
            .await
            .unwrap_err();
        assert!(matches!(error, super::SteveError::StreamingNotSupported));
        assert!(
            slow_headers.is_empty(),
            "stale headers survived a pre-response failure"
        );
        server.abort();
    }

    #[tokio::test]
    async fn cache_directives_belong_to_final_chat_attempt() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        for final_has_cache_header in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let fixture_calls = calls.clone();
            let app = Router::new().route(
                "/v1/chat/completions",
                post(move || {
                    let first = fixture_calls.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        let mut headers = HeaderMap::new();
                        if first {
                            headers.insert("cache-control", HeaderValue::from_static("no-store"));
                        } else if final_has_cache_header {
                            headers.insert(
                                "cache-control",
                                HeaderValue::from_static("private, max-age=0"),
                            );
                        }
                        fixture(
                            headers,
                            false,
                            if first {
                                StatusCode::SERVICE_UNAVAILABLE
                            } else {
                                StatusCode::OK
                            },
                            false,
                        )
                        .await
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let upstream =
                OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2)).unwrap();
            let reply = openai_chat::handle_chat_completions_with_upstream(
                br#"{"model":"m","messages":[{"role":"user"}]}"#,
                &upstream,
            )
            .await;
            assert_eq!(reply.attempts.len(), 2);
            let response = reply.into_response();
            if final_has_cache_header {
                assert_eq!(response.headers()["cache-control"], "private, max-age=0");
            } else {
                assert!(!response.headers().contains_key("cache-control"));
            }
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            response.into_body().collect().await.unwrap();
            server.abort();
        }
    }

    async fn fixture(
        headers: HeaderMap,
        stream: bool,
        status: StatusCode,
        invalid: bool,
    ) -> Response {
        let body = if invalid {
            Body::from("not-json")
        } else if stream {
            Body::from("data: {}\n\ndata: [DONE]\n\n")
        } else {
            Body::from(r#"{"id":"fixture"}"#)
        };
        let mut response = (status, body).into_response();
        *response.headers_mut() = headers;
        response.headers_mut().insert(
            "content-type",
            HeaderValue::from_static(if stream && !invalid {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        response
    }

    #[tokio::test]
    async fn provider_cache_directives_survive_json_sse_and_errors() {
        for invalid in [false, true] {
            for stream in [false, true] {
                for status in [StatusCode::OK, StatusCode::BAD_REQUEST] {
                    let mut headers = HeaderMap::new();
                    headers.append("cache-control", HeaderValue::from_static("private"));
                    headers.append("cache-control", HeaderValue::from_static("no-store"));
                    headers.insert("expires", HeaderValue::from_static("0"));
                    headers.insert("pragma", HeaderValue::from_static("no-cache"));
                    headers.insert("set-cookie", HeaderValue::from_static("secret=yes"));
                    headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
                    headers.insert("x-api-key", HeaderValue::from_static("secret"));
                    let app = Router::new()
                        .route(
                            "/v1/chat/completions",
                            post({
                                let h = headers.clone();
                                move || fixture(h.clone(), stream, status, invalid)
                            }),
                        )
                        .route(
                            "/v1/responses",
                            post({
                                let h = headers.clone();
                                move || fixture(h.clone(), stream, status, invalid)
                            }),
                        )
                        .route(
                            "/v1/messages",
                            post({
                                let h = headers.clone();
                                move || fixture(h.clone(), stream, status, invalid)
                            }),
                        );
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let addr = listener.local_addr().unwrap();
                    let server = tokio::spawn(async move {
                        axum::serve(listener, app).await.unwrap();
                    });
                    let openai =
                        OpenAiUpstream::new(format!("http://{addr}"), Duration::from_secs(2))
                            .unwrap();
                    let anthropic =
                        AnthropicUpstream::new(format!("http://{addr}"), Duration::from_secs(2))
                            .unwrap();
                    let chat = serde_json::to_vec(&json!({"model":"m","messages":[{"role":"user","content":"hi"}],"stream":stream})).unwrap();
                    let responses =
                        serde_json::to_vec(&json!({"model":"m","input":"hi","stream":stream}))
                            .unwrap();
                    let messages = serde_json::to_vec(&json!({"model":"m","messages":[{"role":"user","content":"hi"}],"max_tokens":5,"stream":stream})).unwrap();
                    for response in [
                        openai_chat::handle_chat_completions_with_upstream(&chat, &openai)
                            .await
                            .into_response(),
                        openai_responses::handle_responses_with_upstream(&responses, &openai)
                            .await
                            .into_response(),
                        anthropic_messages::handle_messages_with_upstream(&messages, &anthropic)
                            .await
                            .into_response(),
                    ] {
                        assert_eq!(
                            response.status(),
                            if status.is_success() && !invalid {
                                StatusCode::OK
                            } else {
                                StatusCode::BAD_GATEWAY
                            }
                        );
                        let values: Vec<_> = response
                            .headers()
                            .get_all("cache-control")
                            .iter()
                            .map(|v| v.to_str().unwrap())
                            .collect();
                        assert_eq!(
                            values,
                            ["private", "no-store"],
                            "provider directives lost for stream={stream} upstream={status}"
                        );
                        assert_eq!(response.headers()["expires"], "0");
                        assert_eq!(response.headers()["pragma"], "no-cache");
                        for name in ["set-cookie", "authorization", "x-api-key", "connection"] {
                            assert!(!response.headers().contains_key(name));
                        }
                        response.into_body().collect().await.unwrap();
                    }
                    server.abort();
                }
            }
        }
    }
}
