use invokr_common::{
    cache::{ConfigCache, SecretCache},
    config::AppConfig,
    db::{self, DbContext},
    metrics as m,
    tenant::SchemaProvider,
};
use reqwest::Client;
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::health::WorkerHealth;
use crate::pipeline::{self, PipelineContext};

pub async fn run<S: SchemaProvider>(
    pool: PgPool,
    config: AppConfig,
    schema_provider: S,
    cancel: CancellationToken,
    health: Arc<WorkerHealth>,
) -> anyhow::Result<()> {
    let worker_id = format!("worker_{}", Uuid::new_v4().simple());
    let semaphore = health.semaphore();
    let poll_interval = Duration::from_millis(config.worker.poll_interval_ms);
    let schema_provider = Arc::new(schema_provider);

    let ctx = Arc::new(PipelineContext {
        pool: pool.clone(),
        http_client: Client::new(),
        config_cache: ConfigCache::new(config.worker.config_cache_ttl_sec),
        secret_cache: SecretCache::new(config.worker.secret_cache_ttl_sec),
        encryption_key: config.crypto.encryption_key.clone(),
        table_prefix: config.db.table_prefix.clone(),
    });

    tracing::info!(worker_id = %worker_id, "Worker polling started");

    let idle = Arc::new(AtomicBool::new(false));

    // Single-threaded poller loop with bounded concurrency.
    // The semaphore (max_concurrent permits) gates how many tasks run in parallel.
    // The loop spins freely while permits are available, only sleeping when the
    // previous iteration found no work (idle backoff). Each spawned task holds a
    // permit and releases it on completion, unblocking the next iteration.
    loop {
        // TODO(LATER): handle incase if long-running task in future
        health.tick();

        if idle.load(Ordering::Relaxed) {
            tokio::select! {
                _ = tokio::time::sleep(poll_interval) => {
                    idle.store(false, Ordering::Relaxed);
                }
                _ = cancel.cancelled() => {
                    break;
                }
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }
            permit = semaphore.clone().acquire_owned() => {
                let permit = permit?;

                let schemas = match schema_provider.get_active_schemas().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("Failed to fetch active schemas: {}", e);
                        drop(permit);
                        tokio::select! {
                            _ = tokio::time::sleep(poll_interval) => {}
                            _ = cancel.cancelled() => { break; }
                        }
                        continue;
                    }
                };

                let pool = pool.clone();
                let ctx = ctx.clone();
                let wid = worker_id.clone();
                let idle = idle.clone();

                tokio::spawn(async move {
                    let found = claim_and_process(&pool, &ctx, &schemas, &wid).await;
                    if !found {
                        metrics::counter!(m::WORKER_POLL_IDLE_TOTAL,
                            "worker_id" => wid,
                        )
                        .increment(1);
                        idle.store(true, Ordering::Relaxed);
                    }
                    drop(permit);
                });
            }
        }
    }

    tracing::info!("Shutting down worker, waiting for in-flight tasks...");
    let timeout = Duration::from_secs(config.worker.shutdown_timeout_sec);
    let _ = tokio::time::timeout(timeout, async {
        let _all = semaphore
            .acquire_many(config.worker.max_concurrent as u32)
            .await;
    })
    .await;
    tracing::info!("Worker shutdown complete");
    Ok(())
}

async fn claim_and_process(
    pool: &PgPool,
    ctx: &PipelineContext,
    schemas: &[String],
    worker_id: &str,
) -> bool {
    let prefix = ctx.table_prefix.as_str();

    for schema_name in schemas {
        let mut tx = match db::scoped::scoped_transaction(pool, schema_name).await {
            Ok(tx) => tx,
            Err(e) => {
                tracing::error!(schema = %schema_name, "Failed to begin scoped transaction: {}", e);
                continue;
            }
        };

        // Bundle connection + prefix into a DbContext.
        // NLL ensures the borrow of tx via db is released after the last use
        // of db (process_execution), allowing tx.commit() below.
        let mut db = DbContext::new(&mut tx, prefix);

        let exec = match db::executions::claim(&mut db, worker_id).await {
            Ok(Some(exec)) => exec,
            Ok(None) => continue,
            Err(e) => {
                tracing::error!(schema = %schema_name, "Failed to claim execution: {}", e);
                continue;
            }
        };

        let job = match db::jobs::get(&mut db, &exec.job_id).await {
            Ok(Some(job)) => job,
            Ok(None) => {
                tracing::error!(schema = %schema_name, "Associated job for execution {} not found", exec.execution_id);
                continue;
            }
            Err(e) => {
                tracing::error!(schema = %schema_name, "Failed to fetch associated job: {}", e);
                tracing::warn!(schema = %schema_name, "Marking execution as failed: {}", e);
                let _ = db::executions::complete_failed(&mut db, &exec.execution_id).await;
                continue;
            }
        };

        metrics::counter!(m::EXECUTIONS_CLAIMED_TOTAL,
            "schema" => schema_name.clone(),
            "endpoint_type" => exec.endpoint_type.clone(),
        )
        .increment(1);

        metrics::gauge!(m::WORKER_INFLIGHT, "worker_id" => worker_id.to_string()).increment(1.0);

        let idempotency_key: &str = job
            .idempotency_key
            .as_deref()
            .unwrap_or(exec.execution_id.as_str());

        pipeline::process_execution(
            ctx,
            &mut db,
            schema_name,
            &exec.execution_id,
            idempotency_key,
            &exec.job_id,
            &exec.endpoint,
            &exec.endpoint_type,
            exec.input.as_ref(),
            exec.attempt_count,
            exec.max_attempts,
        )
        .await;

        // db last used above; NLL releases the borrow of tx here
        if let Err(e) = tx.commit().await {
            tracing::error!(
                execution_id = %exec.execution_id,
                "Failed to commit transaction: {}", e
            );
        }

        metrics::gauge!(m::WORKER_INFLIGHT, "worker_id" => worker_id.to_string()).decrement(1.0);

        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::WorkerHealth;
    use invokr_common::config::{
        AppConfig, CryptoEnv, DbEnv, MetricsEnv, ReaperEnv, ServerEnv, ServerMode, WorkerEnv,
        WorkerHealthEnv,
    };
    use invokr_common::tenant::SchemaProvider;
    use sqlx::postgres::PgPoolOptions;

    /// Fails every fetch, which drives the loop down its schema-error branch —
    /// the one iteration path that never touches the database, letting the tick
    /// be observed without one.
    struct FailingSchemas;

    impl SchemaProvider for FailingSchemas {
        async fn get_active_schemas(&self) -> Result<Vec<String>, sqlx::Error> {
            Err(sqlx::Error::PoolClosed)
        }
    }

    fn test_config(poll_interval_ms: u64) -> AppConfig {
        AppConfig {
            db: DbEnv {
                url: "postgres://invokr:invokr@127.0.0.1:1/invokr_db".into(),
                pool_size: 1,
                table_prefix: String::new(),
            },
            server: ServerEnv {
                listen_addr: "0.0.0.0:0".into(),
                api_key: "test".into(),
                path_prefix: String::new(),
                mode: ServerMode::Api,
                dashboard_prefix: String::new(),
                dashboard_dist_dir: String::new(),
            },
            worker: WorkerEnv {
                max_concurrent: 4,
                poll_interval_ms,
                config_cache_ttl_sec: 60,
                secret_cache_ttl_sec: 300,
                shutdown_timeout_sec: 1,
            },
            crypto: CryptoEnv {
                encryption_key: "0".repeat(64),
            },
            metrics: MetricsEnv { port: 0 },
            health: WorkerHealthEnv {
                db_probe_timeout_ms: 200,
                stale_after_floor_ms: 5000,
                server_workers: 1,
            },
            reaper: ReaperEnv {
                cron_expression: "* * * * *".into(),
            },
        }
    }

    #[tokio::test]
    async fn poll_loop_keeps_ticking_health() {
        let health = Arc::new(WorkerHealth::new(
            4,
            Duration::from_millis(20),
            Duration::from_secs(5),
        ));
        let cancel = CancellationToken::new();
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("postgres://invokr:invokr@127.0.0.1:1/invokr_db")
            .unwrap();

        let handle = tokio::spawn(run(
            pool,
            test_config(20),
            FailingSchemas,
            cancel.clone(),
            health.clone(),
        ));

        // Long enough that a loop which only ticked at startup has gone stale.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ago = health.last_tick_ago();

        cancel.cancel();
        handle.await.unwrap().unwrap();

        assert!(
            ago < Duration::from_millis(150),
            "poll loop stopped ticking: last tick was {ago:?} ago"
        );
    }
}
