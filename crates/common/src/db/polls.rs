use crate::db::{tbl, DbContext};
use crate::models::PollClassification;
use chrono::{DateTime, Utc};

#[allow(clippy::too_many_arguments)]
pub async fn insert(
    db: &mut DbContext<'_>,
    execution_id: &str,
    poll_number: i32,
    polled_at: DateTime<Utc>,
    duration_ms: Option<i64>,
    status_code: Option<i32>,
    retry_after_ms: Option<i64>,
    classification: PollClassification,
    error: Option<&serde_json::Value>,
) -> Result<crate::models::Poll, sqlx::Error> {
    let t = tbl(db.prefix, "polls");
    sqlx::query_as::<_, crate::models::Poll>(&format!(
        "INSERT INTO {t}
         (execution_id, poll_number, polled_at, duration_ms,
          status_code, retry_after_ms, classification, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING *"
    ))
    .bind(execution_id)
    .bind(poll_number)
    .bind(polled_at)
    .bind(duration_ms)
    .bind(status_code)
    .bind(retry_after_ms)
    .bind(classification.as_str())
    .bind(error)
    .fetch_one(&mut *db.conn)
    .await
}

pub async fn list_for_execution(
    db: &mut DbContext<'_>,
    execution_id: &str,
) -> Result<Vec<crate::models::Poll>, sqlx::Error> {
    let t = tbl(db.prefix, "polls");
    sqlx::query_as::<_, crate::models::Poll>(&format!(
        "SELECT * FROM {t} WHERE execution_id = $1 ORDER BY poll_number ASC"
    ))
    .bind(execution_id)
    .fetch_all(&mut *db.conn)
    .await
}

/// Number of polls at the end of the sequence classified TRANSIENT_ERROR.
/// Resets to zero as soon as any other classification appears.
pub async fn consecutive_transient_errors(
    db: &mut DbContext<'_>,
    execution_id: &str,
) -> Result<i64, sqlx::Error> {
    let t = tbl(db.prefix, "polls");
    sqlx::query_scalar::<_, i64>(&format!(
        "SELECT COUNT(*) FROM {t}
         WHERE execution_id = $1
           AND poll_number > COALESCE(
               (SELECT MAX(poll_number) FROM {t}
                WHERE execution_id = $1 AND classification <> 'TRANSIENT_ERROR'), 0)"
    ))
    .bind(execution_id)
    .fetch_one(&mut *db.conn)
    .await
}
