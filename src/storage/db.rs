use crate::config::DatabaseConfig;
use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{
    postgres::PgPoolOptions,
    sqlite::SqlitePoolOptions,
    PgPool, SqlitePool,
};
use std::path::Path;

#[derive(Clone)]
pub enum DatabasePool {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

#[derive(Clone)]
pub struct Database {
    hot: DatabasePool,
    background: DatabasePool,
}

impl Database {
    pub async fn connect(cfg: &DatabaseConfig) -> Result<Self> {
        prepare_sqlite_path(&cfg.url)?;

        let (hot, background) = if cfg.url.starts_with("sqlite:") {
            let hot = SqlitePoolOptions::new()
                .max_connections(cfg.hot_max_connections)
                .connect(&cfg.url)
                .await
                .context("connecting hot-path SQLite pool")?;
            let background = SqlitePoolOptions::new()
                .max_connections(cfg.background_max_connections)
                .connect(&cfg.url)
                .await
                .context("connecting background SQLite pool")?;

            configure_sqlite(&hot).await?;
            configure_sqlite(&background).await?;

            (
                DatabasePool::Sqlite(hot),
                DatabasePool::Sqlite(background),
            )
        } else if cfg.url.starts_with("postgres://") || cfg.url.starts_with("postgresql://") {
            let hot = PgPoolOptions::new()
                .max_connections(cfg.hot_max_connections)
                .connect(&cfg.url)
                .await
                .context("connecting hot-path PostgreSQL pool")?;
            let background = PgPoolOptions::new()
                .max_connections(cfg.background_max_connections)
                .connect(&cfg.url)
                .await
                .context("connecting background PostgreSQL pool")?;

            (
                DatabasePool::Postgres(hot),
                DatabasePool::Postgres(background),
            )
        } else {
            anyhow::bail!("unsupported database URL scheme");
        };

        Ok(Self { hot, background })
    }

    pub fn background(&self) -> DatabasePool {
        self.background.clone()
    }

    pub async fn migrate(&self) -> Result<()> {
        match &self.hot {
            DatabasePool::Sqlite(pool) => migrate_sqlite(pool).await,
            DatabasePool::Postgres(pool) => migrate_postgres(pool).await,
        }
    }
}

impl DatabasePool {
    pub async fn insert_background_event(
        &self,
        id: &str,
        kind: &str,
        payload: &str,
        created_at: &str,
    ) -> Result<()> {
        match self {
            Self::Sqlite(pool) => {
                sqlx::query(
                    "INSERT INTO steve_background_events(id, kind, payload, created_at)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT(id) DO NOTHING",
                )
                .bind(id)
                .bind(kind)
                .bind(payload)
                .bind(created_at)
                .execute(pool)
                .await?;
            }
            Self::Postgres(pool) => {
                sqlx::query(
                    "INSERT INTO steve_background_events(id, kind, payload, created_at)
                     VALUES ($1, $2, $3, $4)
                     ON CONFLICT(id) DO NOTHING",
                )
                .bind(id)
                .bind(kind)
                .bind(payload)
                .bind(created_at)
                .execute(pool)
                .await?;
            }
        }
        Ok(())
    }
}

async fn configure_sqlite(pool: &SqlitePool) -> Result<()> {
    sqlx::query("PRAGMA journal_mode = WAL").execute(pool).await?;
    sqlx::query("PRAGMA synchronous = NORMAL")
        .execute(pool)
        .await?;
    Ok(())
}

async fn migrate_sqlite(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS steve_schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    let current: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM steve_schema_migrations")
            .fetch_one(pool)
            .await?;

    if current < 1 {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS steve_background_events (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO steve_schema_migrations(version, name, applied_at) VALUES (?, ?, ?)",
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

async fn migrate_postgres(pool: &PgPool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS steve_schema_migrations (
            version BIGINT PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    let current: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM steve_schema_migrations")
            .fetch_one(pool)
            .await?;

    if current < 1 {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS steve_background_events (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO steve_schema_migrations(version, name, applied_at) VALUES ($1, $2, $3)",
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

fn prepare_sqlite_path(url: &str) -> Result<()> {
    if !url.starts_with("sqlite://") {
        return Ok(());
    }

    let path = url
        .trim_start_matches("sqlite://")
        .split('?')
        .next()
        .unwrap_or_default();

    if path.is_empty() || path == ":memory:" {
        return Ok(());
    }

    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    Ok(())
}
