use crate::config::DatabaseConfig;
use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{postgres::PgPoolOptions, sqlite::SqlitePoolOptions, PgPool, SqlitePool};
use std::path::Path;

const SQLITE_V1_MIGRATION_NAME: &str = "m0_foundation";

type SqliteColumnManifest = (
    &'static str,
    &'static str,
    i64,
    Option<&'static str>,
    i64,
    i64,
);
type SqliteColumn = (i64, String, String, i64, Option<String>, i64, i64);

const SQLITE_V1_LEDGER_COLUMNS: [SqliteColumnManifest; 3] = [
    ("version", "INTEGER", 0, None, 1, 0),
    ("name", "TEXT", 1, None, 0, 0),
    ("applied_at", "TEXT", 1, None, 0, 0),
];
const SQLITE_V1_APPLICATION_COLUMNS: [SqliteColumnManifest; 4] = [
    ("id", "TEXT", 0, None, 1, 0),
    ("kind", "TEXT", 1, None, 0, 0),
    ("payload", "TEXT", 1, None, 0, 0),
    ("created_at", "TEXT", 1, None, 0, 0),
];

fn expected_sqlite_columns(manifest: &[SqliteColumnManifest]) -> Vec<SqliteColumn> {
    manifest
        .iter()
        .enumerate()
        .map(|(cid, (name, ty, notnull, default, pk, hidden))| {
            (
                cid as i64,
                (*name).to_owned(),
                (*ty).to_owned(),
                *notnull,
                default.map(str::to_owned),
                *pk,
                *hidden,
            )
        })
        .collect()
}

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

            (DatabasePool::Sqlite(hot), DatabasePool::Sqlite(background))
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
    sqlx::query("PRAGMA journal_mode = WAL")
        .execute(pool)
        .await?;
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

    let columns: Vec<SqliteColumn> = sqlx::query_as("PRAGMA table_xinfo(steve_schema_migrations)")
        .fetch_all(pool)
        .await?;
    let expected_columns = expected_sqlite_columns(&SQLITE_V1_LEDGER_COLUMNS);
    // INTEGER PRIMARY KEY DESC has the same columns but is not a rowid alias.
    let ordinary_primary_keys: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_index_list('steve_schema_migrations') WHERE origin = 'pk'",
    )
    .fetch_one(pool)
    .await?;
    if columns != expected_columns || ordinary_primary_keys != 0 {
        anyhow::bail!("SQLite migration ledger column shape does not match v1");
    }

    let ledger: Vec<(i64, String)> =
        sqlx::query_as("SELECT version, name FROM steve_schema_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;
    if !ledger.is_empty()
        && (ledger.len() != 1 || ledger[0].0 != 1 || ledger[0].1 != SQLITE_V1_MIGRATION_NAME)
    {
        anyhow::bail!("SQLite migration ledger is not the known v1 prefix");
    }

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
        .bind(SQLITE_V1_MIGRATION_NAME)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }

    let application_columns: Vec<SqliteColumn> =
        sqlx::query_as("PRAGMA table_xinfo(steve_background_events)")
            .fetch_all(pool)
            .await?;
    if application_columns != expected_sqlite_columns(&SQLITE_V1_APPLICATION_COLUMNS) {
        anyhow::bail!("SQLite application column shape does not match v1");
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
