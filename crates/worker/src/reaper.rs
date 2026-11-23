//! CRON job reaper — dogfooded as a invokr INTERNAL CRON job.
//!
//! pg_cron drives execution materialization, but it has no concept of a job's
//! `cron_ends_at` window — left alone it fires forever. The `cron_ends_at` guard
//! in the pg_cron command stops *new executions* past the window; this reaper
//! handles the *lifecycle*: it periodically flips expired CRON jobs to RETIRED
//! and removes their pg_cron entry so they stop firing no-op inserts entirely.
//!
//! The sweep used to run as a tokio interval task in each worker pod, invisible
//! to the rest of invokr. It is now itself a invokr CRON job: each workspace is
//! provisioned at creation time (see `db::workspaces::provision_reaper`) with
//! an `INTERNAL` endpoint named `invokr.reaper` and a `* * * * *` job whose
//! pg_cron tick materializes an execution into the workspace's own
//! `executions` table. The worker claims it via the normal `SKIP LOCKED` path
//! and the [`crate::dispatcher::internal`] arm calls [`reap_schema`] — same
//! code, but now with attempts, retries, duration, metrics and a dashboard
//! row, all for free.
//!
//! Coordination across pods is therefore implicit: pg_cron inserts exactly one
//! execution per tick per schema, and `claim()` uses `FOR UPDATE SKIP LOCKED`,
//! so exactly one pod ends up running each sweep. The previous advisory lock
//! is no longer needed.

use invokr_common::{db, metrics as m, tenant::TablePrefix};
use sqlx::PgConnection;

/// Retire expired CRON jobs in a single schema and unschedule their pg_cron
/// entries, returning the `job_id`s that were retired. Runs on the caller's
/// connection: when invoked from the worker pipeline that conn is the same
/// scoped transaction the execution status is written to, so retire +
/// unschedule + execution outcome commit (or roll back) together.
///
/// Retiring and unscheduling together means a failed unschedule rolls the whole
/// batch back, to be retried on the next sweep; we never commit a RETIRED job
/// while leaving its pg_cron entry scheduled forever (a permanent leak, since
/// future sweeps only look at ACTIVE jobs). The unschedule is existence-guarded,
/// so an already-removed entry is a no-op rather than an error.
pub async fn reap_schema(
    conn: &mut PgConnection,
    prefix: &TablePrefix,
    schema_name: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let retired = db::jobs::retire_expired_cron_jobs(conn, prefix).await?;

    for job_id in &retired {
        db::jobs::unregister_pg_cron_conn(conn, schema_name, job_id).await?;
    }

    if !retired.is_empty() {
        metrics::counter!(m::CRON_JOBS_REAPED_TOTAL, "schema" => schema_name.to_string())
            .increment(retired.len() as u64);
        for job_id in &retired {
            tracing::info!(schema = %schema_name, job_id = %job_id, "Reaped expired CRON job");
        }
    }

    Ok(retired)
}
