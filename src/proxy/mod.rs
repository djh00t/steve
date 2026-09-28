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
