use crate::config::DatabaseConfig;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{
    postgres::PgPoolOptions, sqlite::SqlitePoolOptions, PgPool, Postgres, SqlitePool, Transaction,
};
use std::{fmt::Write as _, io, path::Path};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct BackgroundEventRow {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) payload: String,
    pub(crate) created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum InsertBackgroundEvent {
    Inserted,
    DuplicateIdentical,
    DuplicateConflict {
        existing: BackgroundEventRow,
        verification_id: String,
        backend: String,
        transaction_isolation: String,
    },
    Failed {
        error: String,
    },
    Unknown {
        error: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DatabaseDurabilityProfile {
    pub(crate) backend: String,
    pub(crate) sqlite_journal_mode: Option<String>,
    pub(crate) sqlite_synchronous: Option<String>,
    pub(crate) postgres_fsync: Option<String>,
    pub(crate) postgres_synchronous_commit: Option<String>,
    pub(crate) postgres_full_page_writes: Option<String>,
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
            let hot = sqlite_pool_options()
                .max_connections(cfg.hot_max_connections)
                .connect(&cfg.url)
                .await
                .context("connecting hot-path SQLite pool")?;
            let background = sqlite_pool_options()
                .max_connections(cfg.background_max_connections)
                .connect(&cfg.url)
                .await
                .context("connecting background SQLite pool")?;

            (DatabasePool::Sqlite(hot), DatabasePool::Sqlite(background))
        } else if cfg.url.starts_with("postgres://") || cfg.url.starts_with("postgresql://") {
            let hot = postgres_pool_options()
                .max_connections(cfg.hot_max_connections)
                .connect(&cfg.url)
                .await
                .context("connecting hot-path PostgreSQL pool")?;
            let background = postgres_pool_options()
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
    pub(crate) async fn insert_background_event(
        &self,
        id: &str,
        kind: &str,
        payload: &str,
        created_at: &str,
    ) -> InsertBackgroundEvent {
        match self {
            Self::Sqlite(pool) => {
                insert_sqlite_background_event(pool, id, kind, payload, created_at).await
            }
            Self::Postgres(pool) => {
                insert_postgres_background_event(pool, id, kind, payload, created_at).await
            }
        }
    }

    pub(crate) async fn durability_profile(&self) -> Result<DatabaseDurabilityProfile> {
        match self {
            Self::Sqlite(pool) => sqlite_durability_profile(pool).await,
            Self::Postgres(pool) => postgres_durability_profile(pool).await,
        }
    }
}

fn sqlite_pool_options() -> SqlitePoolOptions {
    SqlitePoolOptions::new().after_connect(|connection, _metadata| {
        Box::pin(async move {
            sqlx::query("PRAGMA journal_mode = WAL")
                .execute(&mut *connection)
                .await?;
            sqlx::query("PRAGMA synchronous = FULL")
                .execute(&mut *connection)
                .await?;
            let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(&mut *connection)
                .await?;
            let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(&mut *connection)
                .await?;
            if !journal_mode.eq_ignore_ascii_case("wal") || synchronous != 2 {
                return Err(configuration_error(format!(
                    "SQLite accounting durability requires WAL/FULL, got {journal_mode}/{synchronous}"
                )));
            }
            Ok(())
        })
    })
}

fn postgres_pool_options() -> PgPoolOptions {
    PgPoolOptions::new().after_connect(|connection, _metadata| {
        Box::pin(async move {
            let fsync: String = sqlx::query_scalar("SHOW fsync")
                .fetch_one(&mut *connection)
                .await?;
            let synchronous_commit: String = sqlx::query_scalar("SHOW synchronous_commit")
                .fetch_one(&mut *connection)
                .await?;
            let full_page_writes: String = sqlx::query_scalar("SHOW full_page_writes")
                .fetch_one(&mut *connection)
                .await?;
            if !is_postgres_on(&fsync)
                || !is_postgres_on(&synchronous_commit)
                || !is_postgres_on(&full_page_writes)
            {
                return Err(configuration_error(format!(
                    "PostgreSQL accounting durability requires fsync=on, synchronous_commit=on, full_page_writes=on; got {fsync}/{synchronous_commit}/{full_page_writes}"
                )));
            }
            Ok(())
        })
    })
}

fn configuration_error(message: String) -> sqlx::Error {
    sqlx::Error::Configuration(Box::new(io::Error::other(message)))
}

fn is_postgres_on(value: &str) -> bool {
    value.eq_ignore_ascii_case("on")
}

async fn sqlite_durability_profile(pool: &SqlitePool) -> Result<DatabaseDurabilityProfile> {
    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(pool)
        .await?;
    let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(pool)
        .await?;
    if !journal_mode.eq_ignore_ascii_case("wal") || synchronous != 2 {
        bail!("SQLite accounting durability requires WAL/FULL, got {journal_mode}/{synchronous}");
    }
    Ok(DatabaseDurabilityProfile {
        backend: "sqlite".into(),
        sqlite_journal_mode: Some("wal".into()),
        sqlite_synchronous: Some("FULL".into()),
        postgres_fsync: None,
        postgres_synchronous_commit: None,
        postgres_full_page_writes: None,
    })
}

async fn postgres_durability_profile(pool: &PgPool) -> Result<DatabaseDurabilityProfile> {
    let fsync: String = sqlx::query_scalar("SHOW fsync").fetch_one(pool).await?;
    let synchronous_commit: String = sqlx::query_scalar("SHOW synchronous_commit")
        .fetch_one(pool)
        .await?;
    let full_page_writes: String = sqlx::query_scalar("SHOW full_page_writes")
        .fetch_one(pool)
        .await?;
    if !is_postgres_on(&fsync)
        || !is_postgres_on(&synchronous_commit)
        || !is_postgres_on(&full_page_writes)
    {
        bail!(
            "PostgreSQL accounting durability requires fsync=on, synchronous_commit=on, full_page_writes=on; got {fsync}/{synchronous_commit}/{full_page_writes}"
        );
    }
    Ok(DatabaseDurabilityProfile {
        backend: "postgres".into(),
        sqlite_journal_mode: None,
        sqlite_synchronous: None,
        postgres_fsync: Some("on".into()),
        postgres_synchronous_commit: Some("on".into()),
        postgres_full_page_writes: Some("on".into()),
    })
}

async fn insert_sqlite_background_event(
    pool: &SqlitePool,
    id: &str,
    kind: &str,
    payload: &str,
    created_at: &str,
) -> InsertBackgroundEvent {
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(err) => return failed("beginning SQLite event transaction", err),
    };
    let inserted = match sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(id)
    .bind(kind)
    .bind(payload)
    .bind(created_at)
    .execute(&mut *tx)
    .await
    {
        Ok(result) => result.rows_affected(),
        Err(err) => {
            let _ = tx.rollback().await;
            return failed("inserting SQLite event", err);
        }
    };
    if inserted == 1 {
        return match tx.commit().await {
            Ok(()) => InsertBackgroundEvent::Inserted,
            Err(err) => unknown("committing inserted SQLite event", err),
        };
    }
    if inserted != 0 {
        let _ = tx.rollback().await;
        return failed_message(format!(
            "SQLite event insert affected unexpected row count {inserted}"
        ));
    }

    let existing = match sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT id, kind, payload, created_at
         FROM steve_background_events WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(Some(row)) => BackgroundEventRow {
            id: row.0,
            kind: row.1,
            payload: row.2,
            created_at: row.3,
        },
        Ok(None) => {
            let _ = tx.rollback().await;
            return unknown_message("SQLite duplicate row disappeared before verification");
        }
        Err(err) => {
            let _ = tx.rollback().await;
            return failed("reading duplicate SQLite event", err);
        }
    };
    let read_uncommitted: i64 = match sqlx::query_scalar("PRAGMA read_uncommitted")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(value) => value,
        Err(err) => {
            let _ = tx.rollback().await;
            return failed("reading SQLite transaction isolation", err);
        }
    };
    let outcome = if row_matches(&existing, id, kind, payload, created_at) {
        InsertBackgroundEvent::DuplicateIdentical
    } else {
        let verification_id =
            conflict_verification_id("sqlite", id, kind, payload, created_at, &existing);
        InsertBackgroundEvent::DuplicateConflict {
            existing,
            verification_id,
            backend: "sqlite".into(),
            transaction_isolation: if read_uncommitted == 0 {
                "serializable".into()
            } else {
                "read_uncommitted".into()
            },
        }
    };
    match tx.commit().await {
        Ok(()) => outcome,
        Err(err) => unknown("committing SQLite duplicate verification", err),
    }
}

async fn insert_postgres_background_event(
    pool: &PgPool,
    id: &str,
    kind: &str,
    payload: &str,
    created_at: &str,
) -> InsertBackgroundEvent {
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(err) => return failed("beginning PostgreSQL event transaction", err),
    };
    let inserted = match sqlx::query(
        "INSERT INTO steve_background_events(id, kind, payload, created_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(id)
    .bind(kind)
    .bind(payload)
    .bind(created_at)
    .execute(&mut *tx)
    .await
    {
        Ok(result) => result.rows_affected(),
        Err(err) => {
            let _ = tx.rollback().await;
            return failed("inserting PostgreSQL event", err);
        }
    };
    if inserted == 1 {
        return match tx.commit().await {
            Ok(()) => InsertBackgroundEvent::Inserted,
            Err(err) => unknown("committing inserted PostgreSQL event", err),
        };
    }
    if inserted != 0 {
        let _ = tx.rollback().await;
        return failed_message(format!(
            "PostgreSQL event insert affected unexpected row count {inserted}"
        ));
    }

    let existing = match select_postgres_background_event_for_verification(&mut tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return unknown_message("PostgreSQL duplicate row disappeared before verification");
        }
        Err(err) => {
            let _ = tx.rollback().await;
            return failed("reading duplicate PostgreSQL event", err);
        }
    };
    let transaction_isolation: String = match sqlx::query_scalar("SHOW transaction_isolation")
        .fetch_one(&mut *tx)
        .await
    {
        Ok(value) => value,
        Err(err) => {
            let _ = tx.rollback().await;
            return failed("reading PostgreSQL transaction isolation", err);
        }
    };
    let outcome = if row_matches(&existing, id, kind, payload, created_at) {
        InsertBackgroundEvent::DuplicateIdentical
    } else {
        let verification_id =
            conflict_verification_id("postgres", id, kind, payload, created_at, &existing);
        InsertBackgroundEvent::DuplicateConflict {
            existing,
            verification_id,
            backend: "postgres".into(),
            transaction_isolation,
        }
    };
    match tx.commit().await {
        Ok(()) => outcome,
        Err(err) => unknown("committing PostgreSQL duplicate verification", err),
    }
}

async fn select_postgres_background_event_for_verification(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<Option<BackgroundEventRow>, sqlx::Error> {
    sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT id, kind, payload, created_at
         FROM steve_background_events WHERE id = $1 FOR SHARE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map(|row| {
        row.map(|row| BackgroundEventRow {
            id: row.0,
            kind: row.1,
            payload: row.2,
            created_at: row.3,
        })
    })
}

fn row_matches(
    row: &BackgroundEventRow,
    id: &str,
    kind: &str,
    payload: &str,
    created_at: &str,
) -> bool {
    row.id == id && row.kind == kind && row.payload == payload && row.created_at == created_at
}

fn conflict_verification_id(
    backend: &str,
    id: &str,
    kind: &str,
    payload: &str,
    created_at: &str,
    existing: &BackgroundEventRow,
) -> String {
    let mut hasher = Sha256::new();
    for field in [
        backend,
        id,
        kind,
        payload,
        created_at,
        &existing.id,
        &existing.kind,
        &existing.payload,
        &existing.created_at,
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    let mut verification_id = String::with_capacity(71);
    verification_id.push_str("sha256:");
    for byte in hasher.finalize() {
        write!(verification_id, "{byte:02x}").expect("writing to String cannot fail");
    }
    verification_id
}

fn failed(context: &str, error: sqlx::Error) -> InsertBackgroundEvent {
    failed_message(format!("{context}: {error}"))
}

fn failed_message(error: impl Into<String>) -> InsertBackgroundEvent {
    InsertBackgroundEvent::Failed {
        error: error.into(),
    }
}

fn unknown(context: &str, error: sqlx::Error) -> InsertBackgroundEvent {
    unknown_message(format!("{context}: {error}"))
}

fn unknown_message(error: impl Into<String>) -> InsertBackgroundEvent {
    InsertBackgroundEvent::Unknown {
        error: error.into(),
    }
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
    let expected_application_columns = expected_sqlite_columns(&SQLITE_V1_APPLICATION_COLUMNS);

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
        let application_columns: Vec<SqliteColumn> =
            sqlx::query_as("PRAGMA table_xinfo(steve_background_events)")
                .fetch_all(&mut *tx)
                .await?;
        if application_columns != expected_application_columns {
            anyhow::bail!("SQLite application column shape does not match v1");
        }
        sqlx::query(
            "INSERT INTO steve_schema_migrations(version, name, applied_at) VALUES (?, ?, ?)",
        )
        .bind(1_i64)
        .bind(SQLITE_V1_MIGRATION_NAME)
        .bind(Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    } else {
        let application_columns: Vec<SqliteColumn> =
            sqlx::query_as("PRAGMA table_xinfo(steve_background_events)")
                .fetch_all(pool)
                .await?;
        if application_columns != expected_application_columns {
            anyhow::bail!("SQLite application column shape does not match v1");
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_insert_classifies_full_content_duplicates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cfg = DatabaseConfig {
            url: format!(
                "sqlite://{}?mode=rwc",
                temp.path().join("events.db").display()
            ),
            ..DatabaseConfig::default()
        };
        let database = Database::connect(&cfg).await.expect("connect database");
        database.migrate().await.expect("migrate database");
        let pool = database.background();
        assert_eq!(
            pool.durability_profile().await.expect("durability profile"),
            DatabaseDurabilityProfile {
                backend: "sqlite".into(),
                sqlite_journal_mode: Some("wal".into()),
                sqlite_synchronous: Some("FULL".into()),
                postgres_fsync: None,
                postgres_synchronous_commit: None,
                postgres_full_page_writes: None,
            }
        );

        assert_eq!(
            pool.insert_background_event("event-1", "test", "{\"value\":1}", "time-1")
                .await,
            InsertBackgroundEvent::Inserted
        );
        assert_eq!(
            pool.insert_background_event("event-1", "test", "{\"value\":1}", "time-1")
                .await,
            InsertBackgroundEvent::DuplicateIdentical
        );
        let InsertBackgroundEvent::DuplicateConflict {
            existing,
            verification_id,
            ..
        } = pool
            .insert_background_event("event-1", "test", "{\"value\":2}", "time-1")
            .await
        else {
            panic!("changed payload was not classified as a conflict");
        };
        assert_eq!(existing.id, "event-1");
        assert_eq!(existing.kind, "test");
        assert_eq!(existing.payload, "{\"value\":1}");
        assert_eq!(existing.created_at, "time-1");
        let InsertBackgroundEvent::DuplicateConflict {
            verification_id: repeated_verification_id,
            ..
        } = pool
            .insert_background_event("event-1", "test", "{\"value\":2}", "time-1")
            .await
        else {
            panic!("repeated changed payload was not classified as a conflict");
        };
        assert_eq!(verification_id, repeated_verification_id);
    }

    #[tokio::test]
    async fn postgres_duplicate_verification_holds_row_lock_until_commit() {
        let Some(database_url) = std::env::var_os("STEVE_TEST_POSTGRES_URL") else {
            eprintln!("STEVE_TEST_POSTGRES_URL is not set; PostgreSQL row-lock case not requested");
            return;
        };
        let database_url = database_url
            .into_string()
            .expect("STEVE_TEST_POSTGRES_URL must be valid UTF-8");
        let pool = postgres_pool_options()
            .max_connections(4)
            .connect(&database_url)
            .await
            .expect("connect configured Task3 PostgreSQL database");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS steve_background_events (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create PostgreSQL event table");
        let event_id = format!("task3-lock-{}", uuid::Uuid::now_v7());
        sqlx::query(
            "INSERT INTO steve_background_events(id, kind, payload, created_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&event_id)
        .bind("test")
        .bind("{\"value\":1}")
        .bind("time-1")
        .execute(&pool)
        .await
        .expect("seed PostgreSQL event");

        let mut verification = pool.begin().await.expect("begin verification transaction");
        let existing =
            select_postgres_background_event_for_verification(&mut verification, &event_id)
                .await
                .expect("lock PostgreSQL event")
                .expect("seeded PostgreSQL event");
        assert!(row_matches(
            &existing,
            &event_id,
            "test",
            "{\"value\":1}",
            "time-1"
        ));
        assert!(!row_matches(
            &existing,
            &event_id,
            "test",
            "{\"value\":2}",
            "time-1"
        ));

        let application_name = format!("steve_task3_lock_{}", uuid::Uuid::now_v7());
        let mut updater = pool.acquire().await.expect("acquire updater connection");
        sqlx::query_scalar::<_, String>("SELECT set_config('application_name', $1, false)")
            .bind(&application_name)
            .fetch_one(&mut *updater)
            .await
            .expect("name updater connection");
        let update_id = event_id.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let update = tokio::spawn(async move {
            let _ = started_tx.send(());
            sqlx::query("UPDATE steve_background_events SET payload = $2 WHERE id = $1")
                .bind(update_id)
                .bind("{\"value\":3}")
                .execute(&mut *updater)
                .await
        });
        started_rx.await.expect("updater task started");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (
                        SELECT 1 FROM pg_stat_activity
                        WHERE application_name = $1 AND wait_event_type = 'Lock'
                    )",
                )
                .bind(&application_name)
                .fetch_one(&pool)
                .await
                .expect("inspect updater lock wait");
                if waiting {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("concurrent update did not wait on the verification row lock");
        assert!(
            !update.is_finished(),
            "update completed before verification commit"
        );

        verification
            .commit()
            .await
            .expect("commit verification transaction");
        tokio::time::timeout(std::time::Duration::from_secs(2), update)
            .await
            .expect("update remained blocked after verification commit")
            .expect("updater task joined")
            .expect("update PostgreSQL event");
        sqlx::query("DELETE FROM steve_background_events WHERE id = $1")
            .bind(&event_id)
            .execute(&pool)
            .await
            .expect("remove PostgreSQL lock fixture");
        pool.close().await;
    }
}
