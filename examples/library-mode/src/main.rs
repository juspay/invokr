//! Library-mode example: embed Invokr directly in a Rust application.
//!
//! Prerequisites:
//!   1. `just setup`         — start PostgreSQL + run base migrations
//!   2. `just mock-server`   — mock HTTP server on port 9999
//!
//! Then:
//!   cargo run -p library-mode-example
//!
//! The example provisions a workspace, registers an HTTP endpoint pointing
//! at the mock server, fires an immediate job, starts the worker, waits for
//! the execution to complete (or Ctrl+C), then shuts down gracefully.

use invokr_common::tenant::SchemaProvider;
use invokr_worker::{InvokrClient, InvokrLibraryClient, JobTrigger, WorkerConfig};
use std::time::Duration;

const DEFAULT_DATABASE_URL: &str = "postgresql://invokr:invokr@localhost:5434/invokr_db";
const SCHEMA_NAME: &str = "library_example";

const ORG_ID: &str = "library";
const WORKSPACE_ID: &str = "example";

// 64 hex chars = 32 bytes = AES-256. Zeros are fine for dev (no real secrets).
// In production: `openssl rand -hex 32`.
const ENCRYPTION_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A minimal `SchemaProvider` that returns a fixed list of schemas.
///
/// In library mode, `provision_workspace()` creates the tenant schema and
/// tables but does not insert into `public.workspaces`. If your host app
/// maintains its own list of active schemas, implement `SchemaProvider`
/// to return them. (Invokr's `SchemaRegistry` queries `public.workspaces`
/// and is used when running Invokr as a standalone service.)
struct StaticSchemaProvider;

impl SchemaProvider for StaticSchemaProvider {
    async fn get_active_schemas(&self) -> Result<Vec<String>, sqlx::Error> {
        Ok(vec![SCHEMA_NAME.to_string()])
    }

    async fn get_org_ws(&self, schema_name: &str) -> Option<(String, String)> {
        (schema_name == SCHEMA_NAME).then(|| (ORG_ID.to_string(), WORKSPACE_ID.to_string()))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let database_url =
        std::env::var("INVOKR_DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
    tracing::info!("connecting to {database_url}");

    // 1. Connect a PgPool and construct the library client.
    //    Pass "" as table_prefix for no prefix (tables: jobs, executions, ...).
    //    Use "sched_" to get sched_jobs, sched_executions, etc.
    let pool = sqlx::PgPool::connect(&database_url).await?;
    let client = InvokrLibraryClient::new(pool, "", ENCRYPTION_KEY, None)?;

    // 2. Provision the workspace schema (idempotent: CREATE SCHEMA IF NOT EXISTS).
    client.provision_workspace(SCHEMA_NAME).await?;
    tracing::info!("provisioned workspace schema '{SCHEMA_NAME}'");

    // 3. Register an HTTP endpoint pointing at the mock server.
    client
        .register_endpoint(
            SCHEMA_NAME,
            "ping",
            "HTTP",
            serde_json::json!({
                "url": "http://localhost:9999/success",
                "method": "POST",
                "headers": { "Content-Type": "application/json" },
                "body_template": { "message": "hello from library mode" },
                "timeout_ms": 5000,
                "expected_status_codes": [200]
            }),
            None,
        )
        .await?;
    tracing::info!("registered endpoint 'ping'");

    // 4. Fire an immediate job. Use a unique idempotency key per run so the
    //    example can be re-run without colliding on the unique index.
    let idempotency_key = format!(
        "example-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let execution_id = client
        .create_job(
            SCHEMA_NAME,
            "ping",
            serde_json::json!({}),
            3, // max_attempts
            JobTrigger::Immediate,
            Some(&idempotency_key),
        )
        .await?;
    tracing::info!("fired job, execution_id={execution_id}");

    // 5. Start the background worker.
    let handle = client.start_worker(StaticSchemaProvider, WorkerConfig::default());
    tracing::info!("worker started; waiting for execution (Ctrl+C to stop)");

    // 6. Poll execution status until terminal, or Ctrl+C.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received Ctrl+C, shutting down");
        }
        _ = wait_for_completion(&client, &execution_id) => {
            tracing::info!("execution reached terminal state");
        }
    }

    // 7. Graceful shutdown: signal cancel, then wait for in-flight jobs to drain.
    handle.shutdown();
    handle.join().await?;
    tracing::info!("worker shut down gracefully");

    Ok(())
}

/// Poll the execution until it reaches a terminal status (SUCCESS / FAILED).
async fn wait_for_completion(client: &InvokrLibraryClient, execution_id: &str) {
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match client.get_execution(SCHEMA_NAME, execution_id).await {
            Ok(Some(exec)) => {
                tracing::info!("execution status: {}", exec.status);
                if exec.status == "SUCCESS" || exec.status == "FAILED" {
                    return;
                }
            }
            Ok(None) => {
                tracing::warn!("execution not found");
                return;
            }
            Err(e) => {
                tracing::error!("failed to fetch execution: {e}");
                return;
            }
        }
    }
}
