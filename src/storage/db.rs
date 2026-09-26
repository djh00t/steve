use crate::config::DatabaseConfig;
use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{any::AnyPoolOptions, AnyPool, Row};
use std::path::Path;

#[derive(Clone)]
pub struct Database {
    hot: AnyPool,
    background: AnyPool,
}

impl Database {
    pub async fn connect(cfg: &DatabaseConfig) -> Result<Self> {
        sqlx::any::install_default_drivers();
        prepare_sqlite_path(&cfg.url)?;
        let hot = AnyPoolOptions::new()
            .max_connections(cfg.hot_max_connections)
            .connect(&cfg.url)
            .await
            .context("connecting hot-path database pool")?;
        let background = AnyPoolOptions::new()
            .max_connections(cfg.background_max_connections)
            .connect(&cfg.url)
            .await
            .context("connecting background database pool")?;
        Ok(Self { hot, background })
    }

    pub fn background(&self) -> &AnyPool { &self.background }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS steve_schema_migrations (
                version BIGINT PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL
            )"
        ).execute(&self.hot).await?;

        let current: i64 = sqlx::query("SELECT COALESCE(MAX(version), 0) FROM steve_schema_migrations")
            .fetch_one(&self.hot).await?
            .try_get(0)?;

        if current < 1 {
            let mut tx = self.hot.begin().await?;
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS steve_background_events (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    created_at TEXT NOT NULL
                )"
            ).execute(&mut *tx).await?;
            sqlx::query(
                "INSERT INTO steve_schema_migrations(version, name, applied_at) VALUES (?, ?, ?)"
            )
            .bind(1_i64)
            .bind("m0_foundation")
            .bind(Utc::now().to_rfc3339())
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        Ok(())
    }
}

fn prepare_sqlite_path(url: &str) -> Result<()> {
    if !url.starts_with("sqlite://") { return Ok(()); }
    let path = url.trim_start_matches("sqlite://").split('?').next().unwrap_or_default();
    if path.is_empty() || path == ":memory:" { return Ok(()); }
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}
