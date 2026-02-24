use clap::Parser;
use ethos_core::EthosConfig;
use tokio::sync::broadcast;
use tracing_subscriber::{fmt, EnvFilter};

use ethos_server::server;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "ethos.toml")]
    config: String,

    #[arg(long)]
    health: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present (dev convenience — production uses real env vars)
    dotenvy::dotenv().ok();

    let args = Args::parse();

    // Init logging
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();

    // Load config
    let config = match EthosConfig::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config from {}: {}", args.config, e);
            std::process::exit(1);
        }
    };

    // Connect to DB
    let pool = match ethos_core::db::create_pool(&config.database).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to connect to database: {}", e);
            std::process::exit(1);
        }
    };

    if args.health {
        match ethos_core::db::health_check(&pool).await {
            Ok(v) => println!("✅ PostgreSQL connected: {}", v),
            Err(e) => {
                println!("❌ PostgreSQL connection failed: {}", e);
                std::process::exit(1);
            }
        }

        match ethos_core::db::check_pgvector(&pool).await {
            Ok(v) => println!("✅ pgvector version: {}", v),
            Err(e) => {
                println!("❌ pgvector check failed: {}", e);
                std::process::exit(1);
            }
        }

        println!("✅ Ethos DB health check passed");
        return Ok(());
    }

    // IPC Server
    let (tx, _rx) = broadcast::channel(1);
    let shutdown_tx = tx.clone();

    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl+C");
        tracing::info!("Shutdown signal received");
        let _ = shutdown_tx.send(());
    });

    // Spawn consolidation background loop (Story 009)
    let consolidation_pool = pool.clone();
    let consolidation_config = config.consolidation.clone();
    let conflict_config = config.conflict_resolution.clone();
    let decay_config = config.decay.clone();
    let consolidation_shutdown = tx.subscribe();

    tokio::spawn(async move {
        ethos_server::subsystems::consolidate::run_consolidation_loop(
            consolidation_pool,
            consolidation_config,
            conflict_config,
            decay_config,
            consolidation_shutdown,
        )
        .await;
    });

    // Spawn re-embed backfill worker (Story 013)
    match ethos_server::subsystems::embedder::create_backend_from_config(&config) {
        Ok(backend) => {
            let reembed_pool = pool.clone();
            let reembed_config = config.embedding.clone();
            let reembed_backend: std::sync::Arc<dyn ethos_core::embeddings::EmbeddingBackend> =
                std::sync::Arc::from(backend);
            tokio::spawn(ethos_server::subsystems::reembed::run_reembed_worker(
                reembed_pool,
                reembed_backend,
                reembed_config,
            ));
        }
        Err(e) => {
            tracing::warn!("Re-embed worker skipped: failed to create embedding backend: {}", e);
        }
    }

    // Spawn HTTP REST API server (Story 011) if enabled
    if config.http.enabled {
        let http_pool = pool.clone();
        let http_config = config.clone();
        let http_shutdown = tx.subscribe();
        tokio::spawn(async move {
            if let Err(e) =
                ethos_server::http::start_http_server(http_pool, http_config, http_shutdown).await
            {
                tracing::error!("HTTP server error: {}", e);
            }
        });
    }

    let socket_path = config.service.socket_path.clone();
    server::run_unix_server(&socket_path, pool, config, tx.subscribe()).await?;

    Ok(())
}
