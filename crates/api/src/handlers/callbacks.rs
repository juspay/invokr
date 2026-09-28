use actix_web::{web, HttpResponse};
use invokr_common::{
    db::{self, scoped, DbContext},
    metrics as m,
};
use serde::Deserialize;
use serde_json::Value;

use crate::extractors::AuthenticatedRequest;
use crate::router::AppState;

/// Callbacks are only honoured when the endpoint's `async.callback.enabled` is
/// true. A missing endpoint or async block counts as disabled.
async fn callback_enabled(db: &mut DbContext<'_>, endpoint: &str) -> bool {
    match db::endpoints::get(db, endpoint).await {
        Ok(Some(ep)) => ep.get_async_config().is_some_and(|c| c.callback),
        _ => false,
    }
}

fn callback_disabled_response() -> HttpResponse {
    HttpResponse::Forbidden().json(serde_json::json!({ "code": "CALLBACK_DISABLED" }))
}

#[derive(Deserialize)]
pub struct CompleteBody {
    pub output: Value,
}

#[derive(Deserialize)]
pub struct FailBody {
    pub error: Value,
}

pub async fn complete(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<CompleteBody>,
) -> HttpResponse {
    let (org_id, workspace_id, execution_id) = path.into_inner();
    let schema_name =
        match db::workspaces::resolve_schema(&state.pool, &org_id, &workspace_id).await {
            Ok(Some(s)) => s,
            Ok(None) => return HttpResponse::Forbidden().finish(),
            Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
        };

    let mut tx = match scoped::scoped_transaction(&state.pool, &schema_name).await {
        Ok(tx) => tx,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    let mut db = DbContext::new(&mut tx, state.prefix());

    match db::executions::get(&mut db, &execution_id).await {
        Ok(Some(e)) if !callback_enabled(&mut db, &e.endpoint).await => {
            return callback_disabled_response();
        }
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
        _ => {}
    }

    let rows_affected = match db::executions::complete_success_from_long_running(
        &mut db,
        &execution_id,
        &body.output,
    )
    .await
    {
        Ok(n) => n,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };

    if rows_affected == 0 {
        let current = db::executions::get(&mut db, &execution_id)
            .await
            .ok()
            .flatten();
        return match current {
            None => HttpResponse::NotFound().finish(),
            Some(e) if matches!(e.status.as_str(), "SUCCESS" | "FAILED" | "CANCELLED") => {
                HttpResponse::Conflict().json(serde_json::json!({
                    "code": "ALREADY_TERMINAL",
                    "current_status": e.status,
                }))
            }
            Some(e) => HttpResponse::Conflict().json(serde_json::json!({
                "code": "NOT_YET_WAITING",
                "current_status": e.status,
            })),
        };
    }

    metrics::counter!(m::CALLBACKS_RECEIVED_TOTAL, "kind" => "complete", "result" => "applied")
        .increment(1);
    metrics::counter!(m::LONG_RUNNING_COMPLETED_TOTAL, "terminator" => "callback", "status" => "SUCCESS")
        .increment(1);
    metrics::gauge!(m::EXECUTIONS_WAITING).decrement(1.0);
    let _ = db::execution_logs::insert(
        &mut db,
        &execution_id,
        0,
        "INFO",
        "Callback received: complete",
    )
    .await;
    let row = db::executions::get(&mut db, &execution_id)
        .await
        .ok()
        .flatten();
    let _ = tx.commit().await;
    match row {
        Some(exec) => HttpResponse::Ok().json(serde_json::json!({ "data": {
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
        }})),
        None => HttpResponse::Ok().finish(),
    }
}

pub async fn fail(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    path: web::Path<(String, String, String)>,
    body: web::Json<FailBody>,
) -> HttpResponse {
    let (org_id, workspace_id, execution_id) = path.into_inner();
    let schema_name =
        match db::workspaces::resolve_schema(&state.pool, &org_id, &workspace_id).await {
            Ok(Some(s)) => s,
            Ok(None) => return HttpResponse::Forbidden().finish(),
            Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
        };

    let mut tx = match scoped::scoped_transaction(&state.pool, &schema_name).await {
        Ok(tx) => tx,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    let mut db = DbContext::new(&mut tx, state.prefix());

    let exec = match db::executions::get(&mut db, &execution_id).await {
        Ok(Some(e)) => e,
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };

    if matches!(exec.status.as_str(), "SUCCESS" | "FAILED" | "CANCELLED") {
        return HttpResponse::Conflict().json(serde_json::json!({
            "code": "ALREADY_TERMINAL",
            "current_status": exec.status,
        }));
    }
    if !callback_enabled(&mut db, &exec.endpoint).await {
        return callback_disabled_response();
    }
    if !matches!(exec.status.as_str(), "WAITING" | "POLLING") {
        return HttpResponse::Conflict().json(serde_json::json!({
            "code": "NOT_YET_WAITING",
            "current_status": exec.status,
        }));
    }

    let endpoint = match db::endpoints::get(&mut db, &exec.endpoint).await {
        Ok(Some(ep)) => ep,
        Ok(None) => return HttpResponse::InternalServerError().body("endpoint missing"),
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    let retry_policy = endpoint.get_retry_policy();
    let backoff_ms = invokr_common::backoff::compute_backoff(&retry_policy, exec.attempt_count);

    let applied = match db::executions::retry_from_long_running(
        &mut db,
        &execution_id,
        backoff_ms,
        &body.error,
    )
    .await
    {
        Ok(rows) => rows > 0,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };

    if !applied {
        // Race-lost: another path finalized this row between our get() and our UPDATE.
        metrics::counter!(m::CALLBACKS_RECEIVED_TOTAL, "kind" => "fail", "result" => "race_lost")
            .increment(1);
        let current = db::executions::get(&mut db, &execution_id)
            .await
            .ok()
            .flatten();
        let _ = tx.commit().await;
        return match current {
            None => HttpResponse::NotFound().finish(),
            Some(e) if matches!(e.status.as_str(), "SUCCESS" | "FAILED" | "CANCELLED") => {
                HttpResponse::Conflict().json(serde_json::json!({
                    "code": "ALREADY_TERMINAL",
                    "current_status": e.status,
                }))
            }
            Some(e) => HttpResponse::Conflict().json(serde_json::json!({
                "code": "RACE_LOST",
                "current_status": e.status,
            })),
        };
    }

    metrics::counter!(m::CALLBACKS_RECEIVED_TOTAL, "kind" => "fail", "result" => "applied")
        .increment(1);
    metrics::counter!(m::LONG_RUNNING_COMPLETED_TOTAL, "terminator" => "callback", "status" => "FAILED")
        .increment(1);
    metrics::gauge!(m::EXECUTIONS_WAITING).decrement(1.0);
    let _ = db::execution_logs::insert(
        &mut db,
        &execution_id,
        0,
        "INFO",
        "Callback received: fail → re-dispatch",
    )
    .await;

    // Re-fetch after the body has been consumed (body is still in scope via `body.error` reference)
    let _ = &body.error; // ensure body is held until here
    let row = db::executions::get(&mut db, &execution_id)
        .await
        .ok()
        .flatten();
    let _ = tx.commit().await;
    match row {
        Some(exec) => HttpResponse::Ok().json(serde_json::json!({ "data": {
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
        }})),
        None => HttpResponse::Ok().finish(),
    }
}
