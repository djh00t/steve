use crate::proxy::{AnthropicUpstream, OpenAiUpstream};
use anyhow::{bail, Result};
use serde::Deserialize;
use std::{collections::HashSet, time::Duration};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeConfig {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub models: Vec<NativeModelConfig>,
    #[serde(default = "reference_tokens")]
    pub reference_input_tokens: u64,
    #[serde(default = "reference_tokens")]
    pub reference_output_tokens: u64,
}

fn reference_tokens() -> u64 {
    1000
}
fn enabled() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderProtocol {
    Openai,
    Anthropic,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NativeProtocol {
    Chat,
    Responses,
    Messages,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub id: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub credential_env: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeModelConfig {
    pub id: String,
    pub upstream_model: String,
    pub provider: String,
    pub protocols: Vec<NativeProtocol>,
    #[serde(default = "enabled")]
    pub streaming: bool,
    pub input_micro_usd_per_million: u64,
    pub output_micro_usd_per_million: u64,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

impl NativeConfig {
    pub fn validate(&self) -> Result<()> {
        if self.reference_input_tokens == 0 && self.reference_output_tokens == 0 {
            bail!("reference token counts must not both be zero");
        }
        let mut ids = HashSet::new();
        for p in &self.providers {
            if p.id.trim().is_empty() || !ids.insert(&p.id) {
                bail!("provider IDs must be nonempty and unique");
            }
            if !valid_env_name(&p.credential_env) {
                bail!("provider credential_env must be an environment variable name");
            }
            let url = reqwest::Url::parse(&p.base_url)
                .map_err(|_| anyhow::anyhow!("invalid provider base URL"))?;
            if url.query().is_some() || url.fragment().is_some() {
                bail!("provider base URL must not have a query or fragment");
            }
            let timeout = Duration::from_secs(30);
            match p.protocol {
                ProviderProtocol::Openai => {
                    OpenAiUpstream::new(&p.base_url, timeout)?;
                }
                ProviderProtocol::Anthropic => {
                    AnthropicUpstream::new(&p.base_url, timeout)?;
                }
            }
        }
        ids.clear();
        for m in &self.models {
            self.score(m).ok_or_else(|| {
                anyhow::anyhow!("model reference cost exceeds integer score range")
            })?;
            if m.id.trim().is_empty() || m.id == "auto" || !ids.insert(&m.id) {
                bail!("model IDs must be nonempty, unique, and not auto");
            }
            if m.upstream_model.trim().is_empty() {
                bail!("upstream model must be nonempty");
            }
            let p = self
                .providers
                .iter()
                .find(|p| p.id == m.provider)
                .ok_or_else(|| anyhow::anyhow!("model references an unknown provider"))?;
            if m.protocols.is_empty()
                || m.protocols.iter().any(|protocol| {
                    !matches!(
                        (p.protocol, protocol),
                        (
                            ProviderProtocol::Openai,
                            NativeProtocol::Chat | NativeProtocol::Responses
                        ) | (ProviderProtocol::Anthropic, NativeProtocol::Messages)
                    )
                })
            {
                bail!("model protocols must match provider protocol");
            }
        }
        Ok(())
    }

    fn score(&self, model: &NativeModelConfig) -> Option<u128> {
        (u128::from(model.input_micro_usd_per_million) * u128::from(self.reference_input_tokens))
            .checked_add(
                u128::from(model.output_micro_usd_per_million)
                    * u128::from(self.reference_output_tokens),
            )
    }
}

impl Default for NativeConfig {
    fn default() -> Self {
        Self {
            providers: vec![],
            models: vec![],
            reference_input_tokens: reference_tokens(),
            reference_output_tokens: reference_tokens(),
        }
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(Clone, Debug)]
pub struct SelectedRoute {
    pub public_model: String,
    pub provider: String,
    pub upstream_model: String,
    pub openai: Option<OpenAiUpstream>,
    pub anthropic: Option<AnthropicUpstream>,
    pub input_micro_usd_per_million: u64,
    pub output_micro_usd_per_million: u64,
}

#[derive(Clone, Debug)]
pub struct NativeRuntime {
    config: NativeConfig,
    routes: Vec<SelectedRoute>,
}

impl NativeRuntime {
    pub fn new(config: &NativeConfig, certificates: &[reqwest::Certificate]) -> Result<Self> {
        config.validate()?;
        let mut routes = vec![];
        for provider in config.providers.iter().filter(|p| p.enabled) {
            let models: Vec<_> = config
                .models
                .iter()
                .filter(|m| m.enabled && m.provider == provider.id)
                .collect();
            if models.is_empty() {
                continue;
            }
            let secret = std::env::var(&provider.credential_env).map_err(|_| {
                anyhow::anyhow!("provider credential environment variable is missing or invalid")
            })?;
            if secret.is_empty() || !secret.bytes().all(|b| b.is_ascii_graphic()) {
                bail!("provider credential must be nonempty printable ASCII without whitespace");
            }
            let mut headers = reqwest::header::HeaderMap::new();
            let (name, value) = match provider.protocol {
                ProviderProtocol::Openai => {
                    (reqwest::header::AUTHORIZATION, format!("Bearer {secret}"))
                }
                ProviderProtocol::Anthropic => (
                    reqwest::header::HeaderName::from_static("x-api-key"),
                    secret,
                ),
            };
            let mut header = reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| anyhow::anyhow!("invalid provider credential header"))?;
            header.set_sensitive(true);
            headers.insert(name, header);
            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .default_headers(headers);
            if provider.protocol == ProviderProtocol::Anthropic {
                builder = builder.pool_max_idle_per_host(0);
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate.clone());
            }
            let client = builder.build()?;
            let timeout = Duration::from_secs(30);
            let (openai, anthropic) = match provider.protocol {
                ProviderProtocol::Openai => (
                    Some(OpenAiUpstream::with_client(
                        &provider.base_url,
                        timeout,
                        client,
                    )?),
                    None,
                ),
                ProviderProtocol::Anthropic => (
                    None,
                    Some(AnthropicUpstream::with_client(
                        &provider.base_url,
                        timeout,
                        client,
                    )?),
                ),
            };
            for model in models {
                routes.push(SelectedRoute {
                    public_model: model.id.clone(),
                    provider: provider.id.clone(),
                    upstream_model: model.upstream_model.clone(),
                    openai: openai.clone(),
                    anthropic: anthropic.clone(),
                    input_micro_usd_per_million: model.input_micro_usd_per_million,
                    output_micro_usd_per_million: model.output_micro_usd_per_million,
                });
            }
        }
        Ok(Self {
            config: config.clone(),
            routes,
        })
    }
    pub fn catalogue(&self) -> Vec<crate::config::ModelConfig> {
        let mut models: Vec<_> = self
            .routes
            .iter()
            .map(|route| crate::config::ModelConfig {
                id: route.public_model.clone(),
                owned_by: route.provider.clone(),
                created: 0,
            })
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }
    pub fn select(
        &self,
        request: &serde_json::Value,
        protocol: NativeProtocol,
    ) -> std::result::Result<SelectedRoute, String> {
        let requested = request
            .get("model")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "model must be a string".to_string())?;
        let streaming = request.get("stream").and_then(serde_json::Value::as_bool) == Some(true);
        let eligible = self.config.models.iter().filter(|model| {
            model.enabled
                && model.protocols.contains(&protocol)
                && (!streaming || model.streaming)
                && self
                    .routes
                    .iter()
                    .any(|route| route.public_model == model.id)
        });
        let model = if requested == "auto" {
            eligible.min_by_key(|model| {
                (
                    self.config.score(model).expect("validated cost score"),
                    &model.id,
                )
            })
        } else {
            eligible.into_iter().find(|model| model.id == requested)
        }
        .ok_or_else(|| {
            "model is unknown, unavailable, or unsupported for this request".to_string()
        })?;
        self.routes
            .iter()
            .find(|route| route.public_model == model.id)
            .cloned()
            .ok_or_else(|| "no eligible model".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> NativeConfig {
        serde_json::from_value(serde_json::json!({
            "providers": [{"id":"p", "protocol":"openai", "base_url":"https://example.com/v1", "credential_env":"STEVE_NATIVE_TEST_KEY"}],
            "models": [{"id":"m", "upstream_model":"up", "provider":"p", "protocols":["chat", "responses"], "input_micro_usd_per_million":1,"output_micro_usd_per_million":2}]
        })).unwrap()
    }

    #[test]
    fn validates_native_configuration() {
        let valid = config();
        assert_eq!(valid.reference_input_tokens, 1000);
        assert!(valid.validate().is_ok());
        let mut zero = valid.clone();
        zero.reference_input_tokens = 0;
        zero.reference_output_tokens = 0;
        assert!(zero.validate().is_err());
        let mut invalid = valid.clone();
        invalid.providers.push(invalid.providers[0].clone());
        assert!(invalid.validate().is_err(), "duplicate provider accepted");
        let mut invalid = valid.clone();
        invalid.models.push(invalid.models[0].clone());
        assert!(invalid.validate().is_err());
        let mut invalid = valid.clone();
        invalid.models[0].provider = "missing".into();
        assert!(invalid.validate().is_err());
        let mut invalid = valid.clone();
        invalid.models[0].protocols = vec![NativeProtocol::Messages];
        assert!(invalid.validate().is_err());
        for url in [
            "https://user:secret@example.com/v1",
            "https://example.com/v1?key=secret",
            "http://example.com/v1",
            "broken",
        ] {
            let mut invalid = valid.clone();
            invalid.providers[0].base_url = url.into();
            let err = invalid.validate().unwrap_err().to_string();
            assert!(!err.contains("secret"));
        }
        let mut invalid = valid;
        invalid.providers[0].credential_env = "secret=value".into();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn runtime_requires_only_enabled_provider_credentials() {
        let mut cfg = config();
        cfg.providers[0].credential_env = "STEVE_NATIVE_MISSING_CREDENTIAL_634".into();
        assert!(
            NativeRuntime::new(&cfg, &[]).is_err(),
            "missing credential accepted"
        );
        cfg.models[0].enabled = false;
        assert!(NativeRuntime::new(&cfg, &[]).is_ok());
        cfg.models[0].enabled = true;
        cfg.providers[0].enabled = false;
        assert!(NativeRuntime::new(&cfg, &[]).is_ok());
    }

    #[tokio::test]
    async fn credential_is_bound_to_server_client_and_redacted() {
        use axum::{http::HeaderMap, routing::post, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|headers: HeaderMap| async move {
                    assert_eq!(headers["authorization"], "Bearer fixture-server-secret");
                    serde_json::json!({"ok":true}).to_string()
                }),
            )
            .route(
                "/v1/messages",
                post(|headers: HeaderMap| async move {
                    assert_eq!(headers["x-api-key"], "fixture-server-secret");
                    assert!(!headers.contains_key("authorization"));
                    assert_eq!(headers["anthropic-version"], "2023-06-01");
                    serde_json::json!({"ok":true}).to_string()
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut cfg = config();
        cfg.providers[0].base_url = format!("http://{addr}");
        cfg.providers[0].credential_env = "STEVE_NATIVE_HEADER_CREDENTIAL_634".into();
        std::env::set_var(&cfg.providers[0].credential_env, "fixture-server-secret");
        let mut anthropic_provider = cfg.providers[0].clone();
        anthropic_provider.id = "anthropic".into();
        anthropic_provider.protocol = ProviderProtocol::Anthropic;
        cfg.providers.push(anthropic_provider);
        let mut anthropic_model = cfg.models[0].clone();
        anthropic_model.id = "claude".into();
        anthropic_model.provider = "anthropic".into();
        anthropic_model.protocols = vec![NativeProtocol::Messages];
        cfg.models.push(anthropic_model);
        let runtime = NativeRuntime::new(&cfg, &[]).unwrap();
        assert!(!format!("{runtime:?}").contains("fixture-server-secret"));
        let upstream = runtime.routes[0].openai.as_ref().unwrap();
        assert_eq!(
            upstream
                .chat_completion(&serde_json::json!({"model":"up"}))
                .await
                .unwrap()["ok"],
            true
        );
        assert_eq!(
            runtime.routes[1]
                .anthropic
                .as_ref()
                .unwrap()
                .create_message(&serde_json::json!({"model":"up","messages":[],"max_tokens":8}))
                .await
                .unwrap()["ok"],
            true
        );
        for value in ["", "line\nbreak", "\t", "non-ascii-\u{00e9}"] {
            std::env::set_var(&cfg.providers[0].credential_env, value);
            let err = NativeRuntime::new(&cfg, &[]).unwrap_err().to_string();
            assert!(!err.contains("line\nbreak"));
        }
        std::env::remove_var(&cfg.providers[0].credential_env);
        server.abort();
    }

    #[test]
    fn selection_filters_capabilities_and_orders_integer_cost_then_id() {
        let mut cfg = config();
        cfg.providers[0].credential_env = "STEVE_NATIVE_SELECTION_CREDENTIAL_634".into();
        std::env::set_var(&cfg.providers[0].credential_env, "fixture");
        let mut other = cfg.models[0].clone();
        other.id = "a".into();
        let mut provider = cfg.providers[0].clone();
        provider.id = "other-provider".into();
        cfg.providers.push(provider);
        other.provider = "other-provider".into();
        cfg.models.push(other.clone());
        other.id = "disabled".into();
        other.enabled = false;
        cfg.models.push(other);
        let runtime = NativeRuntime::new(&cfg, &[]).unwrap();
        let auto = serde_json::json!({"model":"auto"});
        assert_eq!(
            runtime
                .select(&auto, NativeProtocol::Chat)
                .unwrap()
                .public_model,
            "a"
        );
        assert_eq!(
            runtime
                .catalogue()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "m"]
        );
        assert_eq!(
            runtime
                .select(&serde_json::json!({"model":"m"}), NativeProtocol::Responses)
                .unwrap()
                .upstream_model,
            "up"
        );
        for name in ["up", "unknown", "disabled"] {
            assert!(runtime
                .select(&serde_json::json!({"model":name}), NativeProtocol::Chat)
                .is_err());
        }
        assert!(runtime.select(&auto, NativeProtocol::Messages).is_err());
        assert!(runtime
            .select(&serde_json::json!({}), NativeProtocol::Chat)
            .is_err());
        cfg.models[1].streaming = false;
        let runtime = NativeRuntime::new(&cfg, &[]).unwrap();
        assert_eq!(
            runtime
                .select(
                    &serde_json::json!({"model":"auto", "stream":true}),
                    NativeProtocol::Chat
                )
                .unwrap()
                .public_model,
            "m"
        );
        assert!(runtime
            .select(
                &serde_json::json!({"model":"a", "stream":true}),
                NativeProtocol::Chat
            )
            .is_err());
        cfg.models[0].input_micro_usd_per_million = 9_007_199_254_740_992;
        cfg.models[1].input_micro_usd_per_million = 9_007_199_254_740_993;
        let runtime = NativeRuntime::new(&cfg, &[]).unwrap();
        assert_eq!(
            runtime
                .select(&auto, NativeProtocol::Chat)
                .unwrap()
                .public_model,
            "m"
        );
        cfg.models[0].input_micro_usd_per_million = u64::MAX;
        cfg.models[0].output_micro_usd_per_million = u64::MAX;
        cfg.reference_input_tokens = u64::MAX;
        cfg.reference_output_tokens = u64::MAX;
        assert!(cfg.validate().is_err(), "cost score overflow accepted");
        std::env::remove_var(&cfg.providers[0].credential_env);
    }

    #[test]
    fn prices_are_required_unsigned_integers_and_zero_is_free() {
        let mut value = serde_json::json!({"id":"free", "upstream_model":"up", "provider":"p", "protocols":["chat"], "input_micro_usd_per_million":0, "output_micro_usd_per_million":0});
        assert!(serde_json::from_value::<NativeModelConfig>(value.clone()).is_ok());
        value["input_micro_usd_per_million"] = serde_json::json!(-1);
        assert!(serde_json::from_value::<NativeModelConfig>(value.clone()).is_err());
        value
            .as_object_mut()
            .unwrap()
            .remove("input_micro_usd_per_million");
        assert!(serde_json::from_value::<NativeModelConfig>(value).is_err());
    }

    #[test]
    fn rejects_unknown_fields_in_native_configuration() {
        for value in [
            serde_json::json!({"api_key":"do-not-store"}),
            serde_json::json!({"providers":[{"id":"p", "protocol":"openai", "base_url":"https://example.com", "credential_env":"KEY", "api_key":"do-not-store"}]}),
            serde_json::json!({"models":[{"id":"m", "provider":"p", "upstream_model":"up", "protocols":["chat"], "input_micro_usd_per_million":0,"output_micro_usd_per_million":0,"streamng":false}]}),
        ] {
            assert!(serde_json::from_value::<NativeConfig>(value).is_err());
        }
    }
}
