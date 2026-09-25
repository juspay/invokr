use invokr_common::{config::AppConfig, tenant::SchemaRegistry};
use invokr_worker::health::{OpsState, WorkerHealth};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("invokr=debug".parse()?))
        .json()
        .init();

    let config = AppConfig::from_env().await?;
    let pool = invokr_common::db::connect_pool(&config.db.url, config.db.pool_size).await?;

    let metrics_handle = invokr_common::metrics::install_recorder();

    let health = Arc::new(WorkerHealth::new(
        config.worker.max_concurrent,
        Duration::from_millis(config.worker.poll_interval_ms),
        Duration::from_millis(config.health.stale_after_floor_ms),
    ));

    let ops_port = config.metrics.port;
    let ops_state = OpsState {
        pool: pool.clone(),
        health: health.clone(),
        metrics: metrics_handle,
        db_probe_timeout: Duration::from_millis(config.health.db_probe_timeout_ms),
    };
    tokio::spawn(invokr_worker::health::ops_server(
        ops_port,
        config.health.server_workers,
        ops_state,
    )?);

    tracing::info!("Worker starting (/health, /ready, /metrics on port {ops_port})");

    // Standalone: use Invokr's own public.workspaces table for schema discovery.
    let schema_provider = SchemaRegistry::new(pool.clone(), 30);

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        match shutdown_signal().await {
            Ok(signal) => tracing::info!("{signal} received, cancelling worker..."),
            Err(e) => tracing::error!("Signal handler failed ({e}), cancelling worker..."),
        }
        cancel_clone.cancel();
    });

    invokr_worker::poller::run(pool, config, schema_provider, cancel, health).await?;

    Ok(())
}

/// Resolves when the process is asked to shut down, naming the signal.
#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<&'static str> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    tokio::select! {
        _ = sigterm.recv() => Ok("SIGTERM"),
        _ = sigint.recv() => Ok("SIGINT"),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<&'static str> {
    tokio::signal::ctrl_c().await.map(|_| "Ctrl-C")
}
