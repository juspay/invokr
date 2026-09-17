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
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::pipeline::{self, PipelineContext};

pub async fn run<S: SchemaProvider>(
    pool: PgPool,
    config: AppConfig,
    schema_provider: S,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let worker_id = format!("worker_{}", Uuid::new_v4().simple());
    let semaphore = Arc::new(Semaphore::new(config.worker.max_concurrent));
    let poll_interval = Duration::from_millis(config.worker.poll_interval_ms);
    let schema_provider = Arc::new(schema_provider);

    let ctx = Arc::new(PipelineContext {
        pool: pool.clone(),
        http_client: Client::new(),
        config_cache: ConfigCache::new(config.worker.config_cache_ttl_sec),
        secret_cache: SecretCache::new(config.worker.secret_cache_ttl_sec),
        encryption_key: config.crypto.encryption_key.clone(),
        table_prefix: config.db.table_prefix.clone(),
        api_base_url: config.server.api_base_url.clone(),
        path_prefix: config.server.path_prefix.clone(),
    });

    tracing::info!(worker_id = %worker_id, "Worker polling started");

    let idle = Arc::new(AtomicBool::new(false));

    // Single-threaded poller loop with bounded concurrency.
    // The semaphore (max_concurrent permits) gates how many tasks run in parallel.
    // The loop spins freely while permits are available, only sleeping when the
    // previous iteration found no work (idle backoff). Each spawned task holds a
    // permit and releases it on completion, unblocking the next iteration.
    loop {
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

                let sp = Arc::clone(&schema_provider);
                tokio::spawn(async move {
                    let found = claim_and_process(&pool, &ctx, &schemas, &wid, sp).await;
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

async fn claim_and_process<S: SchemaProvider>(
    pool: &PgPool,
    ctx: &PipelineContext,
    schemas: &[String],
    worker_id: &str,
    schema_provider: Arc<S>,
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

        // Resolve org_id / workspace_id for callback URL construction using the
        // cached SchemaProvider — avoids a per-execution DB round-trip since
        // get_active_schemas already refreshed the cache this tick.
        // Falls back to empty strings if the schema is not found (e.g. during a
        // race between schema deletion and claim) so the pipeline can still
        // proceed with degraded (empty/invalid) callback URLs.
        let (org_id, workspace_id) = schema_provider
            .get_org_ws(schema_name)
            .await
            .unwrap_or_default();

        match exec.claim_status.as_str() {
            "RUNNING" => {
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
                    &org_id,
                    &workspace_id,
                    job.async_max_wait_ms,
                    job.async_max_polls,
                )
                .await;
            }
            "POLLING" => {
                crate::poll::process_poll(ctx, &mut db, schema_name, &exec).await;
            }
            other => {
                tracing::error!(execution_id = %exec.execution_id,
                    "Unexpected claim_status {}; failing safe", other);
                let _ = invokr_common::db::executions::complete_failed(&mut db, &exec.execution_id)
                    .await;
            }
        }

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
