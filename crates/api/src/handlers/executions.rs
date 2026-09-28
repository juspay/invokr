use crate::extractors::{AuthenticatedRequest, Workspace};
use crate::router::AppState;
use actix_web::{web, HttpResponse};
use invokr_common::{db, db::DbContext, error::AppError};
use sqlx::PgPool;

pub async fn get(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let execution_id = path.into_inner();
    let exec = db::executions::get(&mut db, &execution_id)
        .await?
        .ok_or(AppError::ExecutionNotFound(execution_id))?;

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": {
        "execution_id": exec.execution_id,
        "job_id": exec.job_id,
        "endpoint": exec.endpoint,
        "endpoint_type": exec.endpoint_type,
        "status": exec.status,
        "input": exec.input,
        "output": exec.output,
        "attempt_count": exec.attempt_count,
        "max_attempts": exec.max_attempts,
        "worker_id": exec.worker_id,
        "run_at": exec.run_at,
        "started_at": exec.started_at,
        "completed_at": exec.completed_at,
        "duration_ms": exec.duration_ms,
        "created_at": exec.created_at,
    }})))
}

pub async fn cancel(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let execution_id = path.into_inner();
    let exec = db::executions::get(&mut db, &execution_id)
        .await?
        .ok_or_else(|| AppError::ExecutionNotFound(execution_id.clone()))?;

    match exec.status.as_str() {
        "PENDING" | "QUEUED" | "WAITING" => {
            let cancelled = db::executions::cancel(&mut db, &execution_id)
                .await?
                .ok_or_else(|| AppError::ExecutionNotCancellable("Could not cancel".into()))?;

            // Best-effort DELETE notification when cancelling a WAITING execution
            if cancelled.previous_status == "WAITING" {
                if let Some(poll_url) = cancelled.poll_url.clone() {
                    let endpoint_name = cancelled.endpoint.clone();
                    let pool = state.pool.clone();
                    let prefix = state.config.db.table_prefix.clone();
                    let schema = ws.0.schema_name.clone();
                    let key = state.config.crypto.encryption_key.clone();
                    let client = reqwest::Client::new();
                    let exec_id = execution_id.clone();
                    tokio::spawn(async move {
                        if let Err(e) = send_cancel_delete(
                            pool,
                            prefix,
                            schema,
                            key,
                            client,
                            endpoint_name,
                            poll_url,
                            exec_id,
                        )
                        .await
                        {
                            tracing::warn!("cancel DELETE failed: {e}");
                        }
                    });
                }
            }

            Ok(HttpResponse::Ok().json(serde_json::json!({ "data": {
                "execution_id": cancelled.execution_id,
                "status": "CANCELLED",
            }})))
        }
        _ => Err(AppError::ExecutionNotCancellable(format!(
            "Execution is in {} state",
            exec.status
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_cancel_delete(
    pool: PgPool,
    prefix: String,
    schema: String,
    encryption_key: String,
    client: reqwest::Client,
    endpoint_name: String,
    poll_url: String,
    execution_id: String,
) -> Result<(), String> {
    let mut tx = invokr_common::db::scoped::scoped_transaction(&pool, &schema)
        .await
        .map_err(|e| e.to_string())?;
    let mut db = invokr_common::db::DbContext::new(&mut tx, &prefix);
    let endpoint = invokr_common::db::endpoints::get(&mut db, &endpoint_name)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "endpoint not found".to_string())?;
    let secret_values =
        invokr_common::secrets::load(&mut db, &encryption_key, &endpoint.spec, None)
            .await
            .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;

    let mut req = client
        .delete(&poll_url)
        .timeout(std::time::Duration::from_secs(5));
    if let Some(headers) = endpoint.spec.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                let mut resolved = s.to_string();
                for (name, val) in &secret_values {
                    resolved = resolved.replace(&format!("{{{{secret.{name}}}}}"), val);
                }
                req = req.header(k.as_str(), resolved);
            }
        }
    }
    let result = req.send().await;

    let mut tx = invokr_common::db::scoped::scoped_transaction(&pool, &schema)
        .await
        .map_err(|e| e.to_string())?;
    let mut db = invokr_common::db::DbContext::new(&mut tx, &prefix);
    let line = match &result {
        Ok(r) => format!("Cancel DELETE to {poll_url} → {}", r.status().as_u16()),
        Err(e) => format!("Cancel DELETE to {poll_url} → error: {e}"),
    };
    let _ =
        invokr_common::db::execution_logs::insert(&mut db, &execution_id, 0, "INFO", &line).await;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn list_attempts(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let execution_id = path.into_inner();
    let _ = db::executions::get(&mut db, &execution_id)
        .await?
        .ok_or_else(|| AppError::ExecutionNotFound(execution_id.clone()))?;

    let attempts = db::attempts::list_for_execution(&mut db, &execution_id).await?;
    let items: Vec<serde_json::Value> = attempts
        .into_iter()
        .map(|a| {
            serde_json::json!({
                "attempt_id": a.attempt_id,
                "attempt_number": a.attempt_number,
                "status": a.status,
                "started_at": a.started_at,
                "completed_at": a.completed_at,
                "duration_ms": a.duration_ms,
                "output": a.output,
                "error": a.error,
            })
        })
        .collect();

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": items })))
}

pub async fn list_polls(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let execution_id = path.into_inner();
    let _ = db::executions::get(&mut db, &execution_id)
        .await?
        .ok_or_else(|| AppError::ExecutionNotFound(execution_id.clone()))?;

    let polls = db::polls::list_for_execution(&mut db, &execution_id).await?;
    let items: Vec<serde_json::Value> = polls
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "execution_id": p.execution_id,
                "poll_number": p.poll_number,
                "polled_at": p.polled_at,
                "duration_ms": p.duration_ms,
                "status_code": p.status_code,
                "retry_after_ms": p.retry_after_ms,
                "classification": p.classification,
                "error": p.error,
            })
        })
        .collect();

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": items })))
}

pub async fn list_logs(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let execution_id = path.into_inner();
    let _ = db::executions::get(&mut db, &execution_id)
        .await?
        .ok_or_else(|| AppError::ExecutionNotFound(execution_id.clone()))?;

    let logs = db::execution_logs::list_for_execution(&mut db, &execution_id).await?;
    let items: Vec<serde_json::Value> = logs
        .into_iter()
        .map(|l| {
            serde_json::json!({
                "log_id": l.log_id,
                "attempt_number": l.attempt_number,
                "level": l.level,
                "message": l.message,
                "logged_at": l.logged_at,
            })
        })
        .collect();

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": items })))
}
