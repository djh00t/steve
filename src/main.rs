mod config;
mod deferred;
mod lifecycle;
mod server;
mod storage;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use deferred::DeferredQueues;
use lifecycle::Lifecycle;
use storage::{Database, ObjectStorage};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "steve", version, about)]
struct Cli {
    #[arg(long, global = true, env = "STEVE_CONFIG")]
    config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve,
    Doctor,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load(cli.config.as_deref())?;
    init_tracing(&cfg);

    match cli.command {
        Command::Serve => serve(cfg).await,
        Command::Doctor => doctor(cfg).await,
    }
}

fn init_tracing(cfg: &Config) {
    let filter = tracing_subscriber::EnvFilter::try_new(&cfg.logging.level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    if cfg.logging.json {
        registry.with(tracing_subscriber::fmt::layer().json()).init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }
}

async fn serve(cfg: Config) -> Result<()> {
    let db = Database::connect(&cfg.database).await?;
    db.migrate().await?;
    let objects = ObjectStorage::from_config(&cfg.object_storage).await?;
    let lifecycle = Lifecycle::new();
    let deferred = DeferredQueues::start(&cfg, db.background().clone(), objects.clone());

    server::run(cfg, db, objects, deferred, lifecycle).await
}

async fn doctor(cfg: Config) -> Result<()> {
    let db = Database::connect(&cfg.database).await?;
    db.migrate().await?;
    let objects = ObjectStorage::from_config(&cfg.object_storage).await?;
    objects.check().await?;
    println!("database: ok");
    println!("object_storage: ok");
    println!("configuration: ok");
    Ok(())
}
