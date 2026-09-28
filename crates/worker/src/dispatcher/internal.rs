//! In-process dispatcher for `INTERNAL` endpoints.
//!
//! `INTERNAL` endpoints back invokr's own dogfooded jobs — work that runs
//! inside the worker pool rather than crossing a network boundary. The
//! endpoint `spec` carries a `task` discriminator; this dispatcher matches on
//! it and calls the matching Rust function, returning its result through the
//! same `DispatchResult` channel as HTTP/Kafka/Redis so the surrounding
//! pipeline (attempts, retries, metrics, dashboard) treats it identically.
//!
//! Today the only task is `"reaper"`: the per-schema CRON sweep that retires
//! expired CRON jobs and unschedules their pg_cron entries. By running it
//! through the regular execution pipeline, every sweep becomes a visible
//! execution in the workspace's own dashboard — self-monitoring via invokr.

use serde_json::Value;
use sqlx::PgConnection;
use std::collections::HashMap;

use crate::dispatcher::DispatchResult;
use crate::reaper;

/// Dispatch an `INTERNAL` execution. `spec` is the endpoint spec after template
/// resolution; the `task` field selects which in-process routine to run. The
/// connection is the same scoped transaction the rest of the pipeline writes
/// to, so the task's effects commit atomically with the execution's outcome.
pub async fn dispatch(
    conn: &mut PgConnection,
    prefix: &str,
    schema_name: &str,
    spec: &Value,
) -> DispatchResult {
    let task = match spec.get("task").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            return DispatchResult::Failure {
                error: serde_json::json!({
                    "type": "INTERNAL_TASK_MISSING",
                    "message": "INTERNAL endpoint spec is missing required `task` field",
                }),
            };
        }
    };

    match task {
        "reaper" => match reaper::reap_schema(conn, prefix, schema_name).await {
            Ok(retired) => DispatchResult::Success {
                output: serde_json::json!({
                    "task": "reaper",
                    "schema": schema_name,
                    "retired_count": retired.len(),
                    "retired_job_ids": retired,
                }),
                headers: HashMap::new(),
                status_code: 0,
            },
            Err(e) => DispatchResult::Failure {
                error: serde_json::json!({
                    "type": "REAPER_SWEEP_FAILED",
                    "message": e.to_string(),
                }),
            },
        },
        other => DispatchResult::Failure {
            error: serde_json::json!({
                "type": "INTERNAL_TASK_UNKNOWN",
                "message": format!("Unknown INTERNAL task: {}", other),
            }),
        },
    }
}
