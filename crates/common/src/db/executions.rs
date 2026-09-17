use crate::{
    db::{tbl, DbContext},
    models::Execution,
};
use chrono::{DateTime, Utc};
use sqlx::prelude::FromRow;

#[derive(FromRow)]
pub struct ClaimedExecution {
    pub execution_id: String,
    pub job_id: String,
    pub endpoint: String,
    pub endpoint_type: String,
    pub input: Option<serde_json::Value>,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub claim_status: String,
    pub poll_url: Option<String>,
    pub poll_count: i32,
    pub max_wait_ms: Option<i64>,
    pub max_polls: Option<i32>,
    pub polling_started_at: Option<DateTime<Utc>>,
    pub polling_deadline: Option<DateTime<Utc>>,
}

pub async fn claim(
    db: &mut DbContext<'_>,
    worker_id: &str,
) -> Result<Option<ClaimedExecution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    let row: Option<ClaimedExecution> = sqlx::query_as(&format!(
        "UPDATE {t} e
         SET
           status = CASE WHEN e.status = 'WAITING' THEN 'POLLING' ELSE 'RUNNING' END,
           worker_id = $1,
           started_at = CASE WHEN e.status = 'WAITING' THEN e.started_at ELSE now() END,
           attempt_count = CASE WHEN e.status = 'WAITING' THEN e.attempt_count
                                ELSE e.attempt_count + 1 END,
           poll_count = CASE WHEN e.status = 'WAITING' THEN e.poll_count + 1
                             ELSE e.poll_count END
         WHERE e.execution_id = (
             SELECT execution_id FROM {t}
             WHERE status IN ('QUEUED','RETRYING','PENDING','WAITING')
               AND run_at <= now()
             ORDER BY run_at ASC
             LIMIT 1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING
           execution_id, job_id, endpoint, endpoint_type, input,
           attempt_count, max_attempts,
           status AS claim_status,
           poll_url, poll_count,
           max_wait_ms, max_polls,
           polling_started_at, polling_deadline"
    ))
    .bind(worker_id)
    .fetch_optional(&mut *db.conn)
    .await?;
    Ok(row)
}

pub async fn complete_success(
    db: &mut DbContext<'_>,
    execution_id: &str,
    output: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'SUCCESS', output = $2, completed_at = now(),
             duration_ms = (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT
         WHERE execution_id = $1 AND status = 'RUNNING'"
    ))
    .bind(execution_id)
    .bind(output)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}

pub async fn complete_retry(
    db: &mut DbContext<'_>,
    execution_id: &str,
    backoff_ms: i64,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = CASE WHEN attempt_count >= max_attempts THEN 'FAILED' ELSE 'RETRYING' END,
             run_at = CASE WHEN attempt_count >= max_attempts THEN run_at
                      ELSE now() + ($2 * interval '1 millisecond') END,
             worker_id = NULL,
             completed_at = CASE WHEN attempt_count >= max_attempts THEN now() ELSE NULL END,
             duration_ms = CASE WHEN attempt_count >= max_attempts
                           THEN (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT
                           ELSE NULL END
         WHERE execution_id = $1 AND status = 'RUNNING'"
    ))
    .bind(execution_id)
    .bind(backoff_ms)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}

pub async fn complete_failed(
    db: &mut DbContext<'_>,
    execution_id: &str,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'FAILED', completed_at = now(),
             duration_ms = (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT,
             worker_id = NULL
         WHERE execution_id = $1 AND status = 'RUNNING'"
    ))
    .bind(execution_id)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}

pub async fn get(
    db: &mut DbContext<'_>,
    execution_id: &str,
) -> Result<Option<Execution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query_as::<_, Execution>(&format!("SELECT * FROM {t} WHERE execution_id = $1"))
        .bind(execution_id)
        .fetch_optional(&mut *db.conn)
        .await
}

pub async fn get_for_job(
    db: &mut DbContext<'_>,
    job_id: &str,
) -> Result<Option<Execution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query_as::<_, Execution>(&format!(
        "SELECT * FROM {t} WHERE job_id = $1 ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(job_id)
    .fetch_optional(&mut *db.conn)
    .await
}

pub async fn list_for_job(
    db: &mut DbContext<'_>,
    job_id: &str,
    cursor: Option<&str>,
    limit: i64,
) -> Result<Vec<Execution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    match cursor {
        Some(c) => {
            sqlx::query_as::<_, Execution>(&format!(
                "SELECT * FROM {t}
                 WHERE job_id = $1 AND created_at < (SELECT created_at FROM {t} WHERE execution_id = $2)
                 ORDER BY created_at DESC LIMIT $3"
            ))
            .bind(job_id)
            .bind(c)
            .bind(limit)
            .fetch_all(&mut *db.conn)
            .await
        }
        None => {
            sqlx::query_as::<_, Execution>(&format!(
                "SELECT * FROM {t} WHERE job_id = $1 ORDER BY created_at DESC LIMIT $2"
            ))
            .bind(job_id)
            .bind(limit)
            .fetch_all(&mut *db.conn)
            .await
        }
    }
}

#[derive(FromRow)]
pub struct CancelledExecution {
    pub execution_id: String,
    pub previous_status: String,
    pub poll_url: Option<String>,
    pub endpoint: String,
}

pub async fn cancel(
    db: &mut DbContext<'_>,
    execution_id: &str,
) -> Result<Option<CancelledExecution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query_as::<_, CancelledExecution>(&format!(
        "WITH cur AS (
            SELECT execution_id, status AS previous_status, poll_url, endpoint
            FROM {t}
            WHERE execution_id = $1
              AND status IN ('PENDING','QUEUED','WAITING')
            FOR UPDATE
         ),
         updated AS (
            UPDATE {t}
            SET status = 'CANCELLED', completed_at = now(), worker_id = NULL
            WHERE execution_id IN (SELECT execution_id FROM cur)
            RETURNING execution_id
         )
         SELECT c.execution_id, c.previous_status, c.poll_url, c.endpoint
         FROM cur c
         JOIN updated u USING (execution_id)"
    ))
    .bind(execution_id)
    .fetch_optional(&mut *db.conn)
    .await
}

pub async fn cancel_pending_for_job(
    db: &mut DbContext<'_>,
    job_id: &str,
) -> Result<Vec<CancelledExecution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query_as::<_, CancelledExecution>(&format!(
        "WITH cur AS (
            SELECT execution_id, status AS previous_status, poll_url, endpoint
            FROM {t}
            WHERE job_id = $1
              AND status IN ('PENDING','QUEUED','WAITING')
            FOR UPDATE
         ),
         updated AS (
            UPDATE {t}
            SET status = 'CANCELLED', completed_at = now(), worker_id = NULL
            WHERE execution_id IN (SELECT execution_id FROM cur)
            RETURNING execution_id
         )
         SELECT c.execution_id, c.previous_status, c.poll_url, c.endpoint
         FROM cur c
         JOIN updated u USING (execution_id)"
    ))
    .bind(job_id)
    .fetch_all(&mut *db.conn)
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn transition_to_waiting(
    db: &mut DbContext<'_>,
    execution_id: &str,
    poll_url: &str,
    polling_started_at: DateTime<Utc>,
    polling_deadline: DateTime<Utc>,
    next_run_at: DateTime<Utc>,
    max_wait_ms: i64,
    max_polls: i32,
) -> Result<u64, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    let res = sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'WAITING',
             poll_url = $2,
             polling_started_at = $3,
             polling_deadline = $4,
             run_at = $5,
             max_wait_ms = $6,
             max_polls = $7,
             poll_count = 0,
             worker_id = NULL
         WHERE execution_id = $1 AND status = 'RUNNING'"
    ))
    .bind(execution_id)
    .bind(poll_url)
    .bind(polling_started_at)
    .bind(polling_deadline)
    .bind(next_run_at)
    .bind(max_wait_ms)
    .bind(max_polls)
    .execute(&mut *db.conn)
    .await?;
    Ok(res.rows_affected())
}

pub async fn transition_back_to_waiting(
    db: &mut DbContext<'_>,
    execution_id: &str,
    next_run_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'WAITING',
             run_at = $2,
             worker_id = NULL
         WHERE execution_id = $1 AND status = 'POLLING'"
    ))
    .bind(execution_id)
    .bind(next_run_at)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}

pub async fn retry_from_poll(
    db: &mut DbContext<'_>,
    execution_id: &str,
    backoff_ms: i64,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = CASE WHEN attempt_count >= max_attempts THEN 'FAILED' ELSE 'RETRYING' END,
             run_at = CASE WHEN attempt_count >= max_attempts THEN run_at
                      ELSE now() + ($2 * interval '1 millisecond') END,
             worker_id = NULL,
             completed_at = CASE WHEN attempt_count >= max_attempts THEN now() ELSE NULL END,
             duration_ms = CASE WHEN attempt_count >= max_attempts
                           THEN (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT
                           ELSE NULL END,
             poll_url = NULL,
             poll_count = 0,
             polling_started_at = NULL,
             polling_deadline = NULL
         WHERE execution_id = $1 AND status = 'POLLING'"
    ))
    .bind(execution_id)
    .bind(backoff_ms)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}

/// Transition a long-running execution from WAITING or POLLING to RETRYING/FAILED.
/// Used by the /fail callback endpoint so a race between WAITING and POLLING doesn't matter.
pub async fn retry_from_long_running(
    db: &mut DbContext<'_>,
    execution_id: &str,
    backoff_ms: i64,
    error: &serde_json::Value,
) -> Result<u64, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    let res = sqlx::query(&format!(
        "UPDATE {t}
         SET status = CASE WHEN attempt_count >= max_attempts THEN 'FAILED' ELSE 'RETRYING' END,
             run_at = CASE WHEN attempt_count >= max_attempts THEN run_at
                      ELSE now() + ($2 * interval '1 millisecond') END,
             worker_id = NULL,
             completed_at = CASE WHEN attempt_count >= max_attempts THEN now() ELSE NULL END,
             duration_ms = CASE WHEN attempt_count >= max_attempts
                           THEN (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT
                           ELSE NULL END,
             poll_url = NULL,
             poll_count = 0,
             polling_started_at = NULL,
             polling_deadline = NULL,
             output = $3
         WHERE execution_id = $1 AND status IN ('WAITING','POLLING')"
    ))
    .bind(execution_id)
    .bind(backoff_ms)
    .bind(error)
    .execute(&mut *db.conn)
    .await?;
    Ok(res.rows_affected())
}

pub async fn complete_failed_timeout(
    db: &mut DbContext<'_>,
    execution_id: &str,
    error: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'FAILED',
             completed_at = now(),
             duration_ms = (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT,
             worker_id = NULL,
             output = $2
         WHERE execution_id = $1 AND status IN ('POLLING','WAITING')"
    ))
    .bind(execution_id)
    .bind(error)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}

pub async fn complete_success_from_long_running(
    db: &mut DbContext<'_>,
    execution_id: &str,
    output: &serde_json::Value,
) -> Result<u64, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    let res = sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'SUCCESS', output = $2, completed_at = now(),
             duration_ms = (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT,
             worker_id = NULL
         WHERE execution_id = $1 AND status IN ('WAITING','POLLING')"
    ))
    .bind(execution_id)
    .bind(output)
    .execute(&mut *db.conn)
    .await?;
    Ok(res.rows_affected())
}
