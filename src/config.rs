use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

const DEFAULT_CONFIG_FILE: &str = "config.toml";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub object_storage: ObjectStorageConfig,
    pub queues: QueueConfig,
    pub logging: LoggingConfig,
    #[serde(skip)]
    source: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub drain_timeout_seconds: u64,
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
            bind: "[::]:11435".into(),
            drain_timeout_seconds: 60,
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
            let mut cfg = Self::default();
            cfg.source = None;
            cfg
        };

        if let Ok(v) = std::env::var("STEVE_BIND") {
            cfg.server.bind = v;
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

        Ok(cfg)
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
    fn default_is_dual_stack() {
        assert_eq!(ServerConfig::default().bind, "[::]:11435");
    }

    #[test]
    fn database_backend_does_not_expose_credentials() {
        let config = DatabaseConfig {
            url: "postgres://user:secret@example/db".into(),
            ..DatabaseConfig::default()
        };
        assert_eq!(config.backend(), "postgres");
    }
}
