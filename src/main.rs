mod accounting;
mod config;
mod deferred;
mod lifecycle;
mod models;
mod net;
mod proxy;
mod server;
mod storage;
mod test_upstream;

use accounting::AccountingCoordinator;
use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use deferred::DeferredQueues;
use lifecycle::Lifecycle;
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use storage::{Database, ObjectStorage};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "steve", version, about)]
struct Cli {
    #[arg(long, global = true, env = "STEVE_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve,
    Doctor,
    Accounting {
        #[command(subcommand)]
        command: AccountingCommand,
    },
    TestUpstream {
        #[arg(long, default_value = "[::]:18080")]
        listen: SocketAddr,
    },
}

#[derive(Subcommand)]
enum AccountingCommand {
    Provision {
        #[arg(long)]
        root: PathBuf,
    },
    AdoptLegacy {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        maintenance_assertion: PathBuf,
    },
    Acknowledge {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        incident: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        evidence_ref: String,
    },
    Audit {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        incident: String,
        #[arg(long)]
        revision: u64,
    },
    ResolveConflict {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        incident: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        event_id: String,
        #[arg(long)]
        authoritative: String,
        #[arg(long)]
        evidence_ref: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load(cli.config.as_deref())?;
    if !matches!(&cli.command, Command::Accounting { .. }) {
        init_tracing(&cfg);
        tracing::info!(
            event = "config_loaded",
            source = %cfg.source_display(),
            inference_bind = %cfg.server.inference_bind,
            management_bind = %cfg.server.management_bind,
            database = %cfg.database.backend(),
            object_storage = %cfg.object_storage.kind,
            "configuration loaded"
        );
    }

    match cli.command {
        Command::Serve => serve(cfg).await,
        Command::Doctor => doctor(cfg).await,
        Command::Accounting { command } => match command {
            AccountingCommand::Provision { root } => {
                AccountingCoordinator::provision(&root)?;
                println!("accounting root provisioned: {}", root.display());
                Ok(())
            }
            AccountingCommand::AdoptLegacy {
                source,
                root,
                maintenance_assertion,
            } => {
                println!(
                    "{}",
                    AccountingCoordinator::adopt_legacy(&source, &root, &maintenance_assertion,)?
                );
                Ok(())
            }
            AccountingCommand::Acknowledge {
                root,
                incident,
                revision,
                kind,
                evidence_ref,
            } => {
                let _offline = AccountingCoordinator::ensure_offline(&root)?;
                let database = Database::connect(&cfg.database).await?;
                database.migrate().await?;
                AccountingCoordinator::reconcile_startup(
                    &root,
                    &database.background(),
                    Duration::from_millis(cfg.queues.accounting_operation_timeout_ms),
                    Duration::from_millis(cfg.queues.accounting_retry_deadline_ms),
                    Duration::from_millis(cfg.queues.accounting_retry_interval_ms),
                    false,
                )
                .await?;
                println!(
                    "{}",
                    serde_json::to_string(&AccountingCoordinator::acknowledge(
                        &root,
                        &incident,
                        revision,
                        &kind,
                        &evidence_ref,
                    )?)?
                );
                Ok(())
            }
            AccountingCommand::Audit {
                root,
                incident,
                revision,
            } => {
                println!(
                    "{}",
                    serde_json::to_string(&AccountingCoordinator::audit(
                        &root, &incident, revision,
                    )?)?
                );
                Ok(())
            }
            AccountingCommand::ResolveConflict {
                root,
                incident,
                revision,
                event_id,
                authoritative,
                evidence_ref,
            } => {
                let _offline = AccountingCoordinator::ensure_offline(&root)?;
                let database = Database::connect(&cfg.database).await?;
                database.migrate().await?;
                let result = AccountingCoordinator::resolve_conflict(
                    &root,
                    &database.background(),
                    &incident,
                    revision,
                    &event_id,
                    &authoritative,
                    &evidence_ref,
                )
                .await?;
                println!("{}", serde_json::to_string(&result)?);
                Ok(())
            }
        },
        Command::TestUpstream { listen } => test_upstream::run(listen).await,
    }
}

fn init_tracing(cfg: &Config) {
    let filter = tracing_subscriber::EnvFilter::try_new(&cfg.logging.level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    if cfg.logging.json {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }
}

async fn serve(cfg: Config) -> Result<()> {
    let db = Database::connect(&cfg.database).await?;
    db.migrate().await?;
    let objects = ObjectStorage::from_config(&cfg.object_storage).await?;
    let lifecycle = Lifecycle::new();
    let deferred = DeferredQueues::start(&cfg, db.background(), objects.clone()).await?;

    server::run(cfg, db, objects, deferred, lifecycle).await
}

async fn doctor(cfg: Config) -> Result<()> {
    let db = Database::connect(&cfg.database).await?;
    db.migrate().await?;
    let objects = ObjectStorage::from_config(&cfg.object_storage).await?;
    objects.check().await?;
    println!("database: ok");
    println!("object_storage: ok");
    println!("configuration: ok ({})", cfg.source_display());
    Ok(())
}
