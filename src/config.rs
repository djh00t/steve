use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

const DEFAULT_CONFIG_FILE: &str = "config.toml";
/// Keeps retry scheduling finite and well within monotonic clock arithmetic.
const MAX_ACCOUNTING_RETRY_DURATION_MS: u64 = 3_600_000;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub object_storage: ObjectStorageConfig,
    pub queues: QueueConfig,
    pub logging: LoggingConfig,
    pub models: Vec<ModelConfig>,
    #[serde(skip)]
    source: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ModelConfig {
    pub id: String,
    #[serde(default = "default_model_owner")]
    pub owned_by: String,
    #[serde(default)]
    pub created: i64,
}

fn default_model_owner() -> String {
    "steve".to_owned()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    #[serde(alias = "bind")]
    pub inference_bind: String,
    pub management_bind: String,
    pub drain_timeout_seconds: u64,
    pub openai_upstream_url: Option<String>,
    pub anthropic_upstream_url: Option<String>,
    pub upstream_ca_bundle: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    pub hot_max_connections: u32,
    pub background_max_connections: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ObjectStorageConfig {
    pub kind: String,
    pub root: String,
    pub bucket: Option<String>,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct QueueConfig {
    pub accounting: usize,
    pub accounting_journal: String,
    pub accounting_journal_queue: usize,
    pub accounting_operation_timeout_ms: u64,
    pub accounting_retry_deadline_ms: u64,
    pub accounting_retry_interval_ms: u64,
    pub history: usize,
    pub telemetry: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
    pub json: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            inference_bind: "[::]:11435".into(),
            management_bind: "[::]:8790".into(),
            drain_timeout_seconds: 60,
            openai_upstream_url: None,
            anthropic_upstream_url: None,
            upstream_ca_bundle: None,
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "sqlite://data/steve.db?mode=rwc".into(),
            hot_max_connections: 8,
            background_max_connections: 4,
        }
    }
}

impl Default for ObjectStorageConfig {
    fn default() -> Self {
        Self {
            kind: "fs".into(),
            root: "data/objects".into(),
            bucket: None,
            endpoint: None,
            region: None,
            access_key_id: None,
            secret_access_key: None,
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            accounting: 4096,
            accounting_journal: "/var/lib/steve/accounting".into(),
            accounting_journal_queue: 1024,
            accounting_operation_timeout_ms: 1_000,
            accounting_retry_deadline_ms: 5_000,
            accounting_retry_interval_ms: 100,
            history: 2048,
            telemetry: 8192,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            json: false,
        }
    }
}

impl Config {
    pub fn load(override_path: Option<&Path>) -> Result<Self> {
        let path = override_path.map(Path::to_path_buf).unwrap_or_else(|| {
            std::env::var_os("STEVE_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_FILE))
        });

        let mut cfg = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("reading config {}", path.display()))?;
            let mut cfg: Self = toml::from_str(&raw).context("parsing config")?;
            cfg.source = Some(path.clone());
            cfg
        } else if override_path.is_some() || std::env::var_os("STEVE_CONFIG").is_some() {
            anyhow::bail!("configuration file not found: {}", path.display());
        } else {
            Self::default()
        };

        if let Ok(v) = std::env::var("STEVE_BIND") {
            cfg.server.inference_bind = v;
        }
        if let Ok(v) = std::env::var("STEVE_INFERENCE_BIND") {
            cfg.server.inference_bind = v;
        }
        if let Ok(v) = std::env::var("STEVE_MANAGEMENT_BIND") {
            cfg.server.management_bind = v;
        }
        if let Ok(v) = std::env::var("STEVE_DATABASE_URL") {
            cfg.database.url = v;
        }
        if let Ok(v) = std::env::var("STEVE_OBJECT_STORE") {
            cfg.object_storage.kind = v;
        }
        if let Ok(v) = std::env::var("STEVE_OBJECT_ROOT") {
            cfg.object_storage.root = v;
        }
        if let Ok(v) = std::env::var("STEVE_S3_BUCKET") {
            cfg.object_storage.bucket = Some(v);
        }
        if let Ok(v) = std::env::var("STEVE_S3_ENDPOINT") {
            cfg.object_storage.endpoint = Some(v);
        }
        if let Ok(v) = std::env::var("STEVE_S3_REGION") {
            cfg.object_storage.region = Some(v);
        }
        if let Ok(v) = std::env::var("STEVE_S3_ACCESS_KEY_ID") {
            cfg.object_storage.access_key_id = Some(v);
        }
        if let Ok(v) = std::env::var("STEVE_S3_SECRET_ACCESS_KEY") {
            cfg.object_storage.secret_access_key = Some(v);
        }
        if let Ok(v) = std::env::var("RUST_LOG") {
            cfg.logging.level = v;
        }

        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let queues = &self.queues;
        if queues.accounting_operation_timeout_ms == 0
            || queues.accounting_retry_deadline_ms == 0
            || queues.accounting_retry_interval_ms == 0
        {
            anyhow::bail!("accounting retry durations must be positive");
        }
        if queues.accounting_operation_timeout_ms > MAX_ACCOUNTING_RETRY_DURATION_MS
            || queues.accounting_retry_deadline_ms > MAX_ACCOUNTING_RETRY_DURATION_MS
            || queues.accounting_retry_interval_ms > MAX_ACCOUNTING_RETRY_DURATION_MS
        {
            anyhow::bail!("accounting retry durations must not exceed 3600000 milliseconds");
        }
        if queues.accounting_operation_timeout_ms > queues.accounting_retry_deadline_ms {
            anyhow::bail!("accounting operation timeout must not exceed retry deadline");
        }
        if queues.accounting_retry_interval_ms > queues.accounting_retry_deadline_ms {
            anyhow::bail!("accounting retry interval must not exceed retry deadline");
        }
        Ok(())
    }

    pub fn source_display(&self) -> String {
        self.source
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "built-in defaults (config.toml not found)".into())
    }
}

impl DatabaseConfig {
    pub fn backend(&self) -> &'static str {
        if self.url.starts_with("postgres://") || self.url.starts_with("postgresql://") {
            "postgres"
        } else if self.url.starts_with("sqlite:") {
            "sqlite"
        } else {
            "unknown"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_dual_stack() {
        let server = ServerConfig::default();
        assert_eq!(server.inference_bind, "[::]:11435");
        assert_eq!(server.management_bind, "[::]:8790");
    }

    #[test]
    fn configured_models_parse_from_toml() {
        let raw = r#"
            [[models]]
            id = "gpt-local"
            owned_by = "lab"
            created = 42

            [[models]]
            id = "bare-model"
        "#;
        let cfg: Config = toml::from_str(raw).expect("parse");
        assert_eq!(
            cfg.models,
            vec![
                ModelConfig {
                    id: "gpt-local".into(),
                    owned_by: "lab".into(),
                    created: 42,
                },
                ModelConfig {
                    id: "bare-model".into(),
                    owned_by: "steve".into(),
                    created: 0,
                },
            ]
        );
    }

    #[test]
    fn missing_models_section_is_empty_until_catalogue_resolution() {
        let cfg = Config::default();
        assert!(cfg.models.is_empty());
    }

    #[test]
    fn database_backend_does_not_expose_credentials() {
        let config = DatabaseConfig {
            url: "postgres://user:secret@example/db".into(),
            ..DatabaseConfig::default()
        };
        assert_eq!(config.backend(), "postgres");
    }

    #[test]
    fn accounting_retry_bounds_are_validated() {
        let mut cfg = Config::default();
        assert!(cfg.validate().is_ok());

        cfg.queues.accounting_operation_timeout_ms = 0;
        assert!(cfg.validate().is_err());
        cfg.queues.accounting_operation_timeout_ms = 6_000;
        assert!(cfg.validate().is_err());

        cfg.queues.accounting_operation_timeout_ms = 1_000;
        cfg.queues.accounting_retry_interval_ms = 6_000;
        assert!(cfg.validate().is_err());

        cfg = Config::default();
        cfg.queues.accounting_retry_deadline_ms = MAX_ACCOUNTING_RETRY_DURATION_MS + 1;
        assert!(cfg.validate().is_err());
        cfg = Config::default();
        cfg.queues.accounting_operation_timeout_ms = MAX_ACCOUNTING_RETRY_DURATION_MS + 1;
        cfg.queues.accounting_retry_deadline_ms = MAX_ACCOUNTING_RETRY_DURATION_MS + 1;
        assert!(cfg.validate().is_err());
        cfg = Config::default();
        cfg.queues.accounting_retry_interval_ms = MAX_ACCOUNTING_RETRY_DURATION_MS + 1;
        cfg.queues.accounting_retry_deadline_ms = MAX_ACCOUNTING_RETRY_DURATION_MS + 1;
        assert!(cfg.validate().is_err());
    }
}
