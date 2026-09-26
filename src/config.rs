use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub object_storage: ObjectStorageConfig,
    pub queues: QueueConfig,
    pub logging: LoggingConfig,
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
        Self { bind: "127.0.0.1:11435".into(), drain_timeout_seconds: 60 }
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
        Self { accounting: 4096, history: 2048, telemetry: 8192 }
    }
}
impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: "info".into(), json: false }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut cfg = if let Some(path) = path {
            let raw = fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?;
            toml::from_str(&raw).context("parsing config")?
        } else {
            Self::default()
        };

        if let Ok(v) = std::env::var("STEVE_BIND") { cfg.server.bind = v; }
        if let Ok(v) = std::env::var("STEVE_DATABASE_URL") { cfg.database.url = v; }
        if let Ok(v) = std::env::var("STEVE_OBJECT_STORE") { cfg.object_storage.kind = v; }
        if let Ok(v) = std::env::var("STEVE_OBJECT_ROOT") { cfg.object_storage.root = v; }
        if let Ok(v) = std::env::var("STEVE_S3_BUCKET") { cfg.object_storage.bucket = Some(v); }
        if let Ok(v) = std::env::var("STEVE_S3_ENDPOINT") { cfg.object_storage.endpoint = Some(v); }
        if let Ok(v) = std::env::var("STEVE_S3_REGION") { cfg.object_storage.region = Some(v); }
        if let Ok(v) = std::env::var("STEVE_S3_ACCESS_KEY_ID") { cfg.object_storage.access_key_id = Some(v); }
        if let Ok(v) = std::env::var("STEVE_S3_SECRET_ACCESS_KEY") { cfg.object_storage.secret_access_key = Some(v); }
        if let Ok(v) = std::env::var("RUST_LOG") { cfg.logging.level = v; }

        Ok(cfg)
    }
}
