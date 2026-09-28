use crate::extractors::{AuthenticatedRequest, JobFilters, Workspace};
use crate::router::AppState;
use actix_web::{web, HttpResponse};
use chrono::Utc;
use invokr_common::metrics as m;
use invokr_common::{
    db,
    db::DbContext,
    error::AppError,
    models::endpoint::EndpointType,
    models::job::{CreateJob, TriggerType, UpdateJob},
    models::pg_cron_expr::PgCronExpr,
    pagination::{encode_cursor, PaginatedResponse, PaginationParams},
};
use uuid::Uuid;

// `drop(db)` below is not about running a destructor — it marks the point
// where the DbContext hands the `&mut tx` borrow back so the transaction
// can be used (and committed) directly. Keep it: NLL would end the borrow
// anyway, but the explicit release is what the surrounding comments refer to.
#[allow(clippy::drop_non_drop)]
pub async fn create(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    body: web::Json<CreateJob>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();

    let trigger = TriggerType::from_str_val(&body.trigger)
        .ok_or_else(|| AppError::InvalidRequest(format!("Invalid trigger: {}", body.trigger)))?;

    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);

    let ep = db::endpoints::get(&mut db, &body.endpoint)
        .await?
        .ok_or_else(|| AppError::EndpointNotFound(body.endpoint.clone()))?;

    // INTERNAL endpoints back invokr-driven jobs (today: the dogfooded reaper)
    // and are not user-creatable — see `handlers::endpoints::create`. The same
    // invariant has to hold on the job side, or a user could stack their own
    // jobs against an internal endpoint (extra reaper sweeps, or one-off
    // IMMEDIATE/DELAYED reaper invocations).
    if EndpointType::from_str_val(&ep.endpoint_type) == Some(EndpointType::INTERNAL) {
        return Err(AppError::InvalidRequest(format!(
            "Endpoint '{}' is internal and cannot be used for user-created jobs",
            body.endpoint
        )));
    }

    let retry_policy = ep.get_retry_policy();
    // Per-job override wins when provided and positive; otherwise the endpoint's policy.
    let max_attempts = body
        .max_attempts
        .filter(|&n| n > 0)
        .unwrap_or(retry_policy.max_attempts);

    let endpoint_async = ep.get_async_config().map(|c| (c.max_wait_ms, c.max_polls));
    let bounds = match invokr_common::models::job::resolve_async_bounds(
        body.async_overrides.as_ref(),
        endpoint_async,
    ) {
        Ok(b) => b,
        Err(msg) => return Err(AppError::InvalidRequest(msg)),
    };
    let (async_max_wait_ms, async_max_polls) = match bounds {
        Some((w, p)) => (Some(w), Some(p)),
        None => (None, None),
    };

    if let Some(ref ps_name) = ep.payload_spec_ref {
        if let Some(ref input) = body.input {
            let spec = db::payload_specs::get(&mut db, ps_name)
                .await?
                .ok_or_else(|| AppError::InvalidPayloadSpecRef(ps_name.clone()))?;
            validate_input(input, &spec.schema_json)?;
        }
    }

    if let Some(ref key) = body.idempotency_key {
        if let Some(existing) = db::jobs::get_by_idempotency(&mut db, &body.endpoint, key).await? {
            let exec = db::executions::get_for_job(&mut db, &existing.job_id).await?;
            return Ok(HttpResponse::Ok()
                .json(serde_json::json!({ "data": job_response(&existing, exec.as_ref()) })));
        }
    }

    // Drop the scoped connection (and its DbContext) before starting transactions
    drop(db);
    drop(conn);

    match trigger {
        TriggerType::IMMEDIATE => {
            let generated_key;
            let key = match body.idempotency_key.as_deref() {
                Some(k) => k,
                None => {
                    generated_key = Uuid::new_v4().to_string();
                    &generated_key
                }
            };

            let mut tx =
                invokr_common::db::scoped::scoped_transaction(&state.pool, &ws.0.schema_name)
                    .await
                    .map_err(AppError::from)?;
            let mut db = DbContext::new(&mut tx, prefix);

            let result = db::jobs::create_immediate(
                &mut db,
                &body.endpoint,
                &ep.endpoint_type,
                key,
                body.input.as_ref(),
                max_attempts,
                async_max_wait_ms,
                async_max_polls,
            )
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
                    AppError::Conflict("Job with this idempotency key already exists".into())
                }
                _ => AppError::from(e),
            })?;

            drop(db);
            tx.commit().await.map_err(AppError::from)?;

            metrics::counter!(m::JOBS_CREATED_TOTAL,
                "trigger_type" => "IMMEDIATE",
                "endpoint" => body.endpoint.clone(),
                "schema" => ws.0.schema_name.clone(),
            )
            .increment(1);

            Ok(HttpResponse::Created().json(serde_json::json!({ "data": {
                "job_id": result.job.job_id,
                "endpoint": result.job.endpoint,
                "endpoint_type": result.job.endpoint_type,
                "trigger": result.job.trigger_type,
                "status": result.job.status,
                "version": result.job.version,
                "idempotency_key": result.job.idempotency_key,
                "input": result.job.input,
                "execution": {
                    "execution_id": result.execution_id,
                    "status": result.execution_status,
                    "created_at": result.execution_created_at,
                },
                "created_at": result.job.created_at,
            }})))
        }
        TriggerType::DELAYED => {
            let key = body.idempotency_key.as_deref().ok_or_else(|| {
                AppError::InvalidRequest("idempotency_key required for DELAYED jobs".into())
            })?;
            let run_at = body.run_at.ok_or_else(|| {
                AppError::InvalidRequest("run_at required for DELAYED jobs".into())
            })?;

            let mut tx =
                invokr_common::db::scoped::scoped_transaction(&state.pool, &ws.0.schema_name)
                    .await
                    .map_err(AppError::from)?;
            let mut db = DbContext::new(&mut tx, prefix);

            let result = db::jobs::create_delayed(
                &mut db,
                &body.endpoint,
                &ep.endpoint_type,
                key,
                body.input.as_ref(),
                run_at,
                max_attempts,
                async_max_wait_ms,
                async_max_polls,
            )
            .await?;

            drop(db);
            tx.commit().await.map_err(AppError::from)?;

            metrics::counter!(m::JOBS_CREATED_TOTAL,
                "trigger_type" => "DELAYED",
                "endpoint" => body.endpoint.clone(),
                "schema" => ws.0.schema_name.clone(),
            )
            .increment(1);

            Ok(HttpResponse::Created().json(serde_json::json!({ "data": {
                "job_id": result.job.job_id,
                "endpoint": result.job.endpoint,
                "endpoint_type": result.job.endpoint_type,
                "trigger": result.job.trigger_type,
                "status": result.job.status,
                "version": result.job.version,
                "idempotency_key": result.job.idempotency_key,
                "input": result.job.input,
                "run_at": result.job.run_at,
                "execution": {
                    "execution_id": result.execution_id,
                    "status": result.execution_status,
                    "created_at": result.execution_created_at,
                },
                "created_at": result.job.created_at,
            }})))
        }
        TriggerType::CRON => {
            let cron_expr = body
                .cron
                .as_ref()
                .ok_or_else(|| AppError::InvalidRequest("cron required for CRON jobs".into()))?;
            let tz_str = body.timezone.as_deref().ok_or_else(|| {
                AppError::InvalidRequest("timezone required for CRON jobs".into())
            })?;

            let schedule = cron_expr.to_schedule();

            let tz: chrono_tz::Tz = tz_str
                .parse()
                .map_err(|_| AppError::InvalidRequest(format!("Invalid timezone: {}", tz_str)))?;

            let starts_at = body.starts_at.unwrap_or_else(Utc::now);
            let next_run = compute_next_cron(&schedule, &tz, starts_at).ok_or_else(|| {
                AppError::InvalidCron("No upcoming run for this cron schedule".into())
            })?;

            let mut tx =
                invokr_common::db::scoped::scoped_transaction(&state.pool, &ws.0.schema_name)
                    .await
                    .map_err(AppError::from)?;
            let mut db = DbContext::new(&mut tx, prefix);

            let job = db::jobs::create_cron(
                &mut db,
                &body.endpoint,
                &ep.endpoint_type,
                body.idempotency_key.as_deref(),
                body.input.as_ref(),
                cron_expr.as_str(),
                tz_str,
                Some(starts_at),
                body.ends_at,
                next_run,
                async_max_wait_ms,
                async_max_polls,
            )
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(ref db_err)
                    if db_err.kind() == sqlx::error::ErrorKind::UniqueViolation =>
                {
                    AppError::Conflict("Job with this idempotency key already exists".into())
                }
                sqlx::Error::Database(ref db_err)
                    if db_err.kind() == sqlx::error::ErrorKind::ForeignKeyViolation =>
                {
                    AppError::EndpointNotFound(body.endpoint.clone())
                }
                _ => AppError::from(e),
            })?;

            drop(db);

            // Register with pg_cron for automatic execution materialization.
            // Run it on the same tx as the row write so the persisted job and
            // the pg_cron schedule commit (or roll back) together. If pg_cron is
            // unhealthy the whole create fails and the client can retry.
            db::jobs::register_pg_cron_conn(
                &mut tx,
                prefix,
                &ws.0.schema_name,
                &job.job_id,
                cron_expr.as_str(),
            )
            .await?;

            tx.commit().await.map_err(AppError::from)?;

            metrics::counter!(m::JOBS_CREATED_TOTAL,
                "trigger_type" => "CRON",
                "endpoint" => body.endpoint.clone(),
                "schema" => ws.0.schema_name.clone(),
            )
            .increment(1);

            Ok(HttpResponse::Created().json(serde_json::json!({ "data": {
                "job_id": job.job_id,
                "endpoint": job.endpoint,
                "endpoint_type": job.endpoint_type,
                "trigger": job.trigger_type,
                "status": job.status,
                "version": job.version,
                "idempotency_key": job.idempotency_key,
                "cron": job.cron_expression,
                "timezone": job.cron_timezone,
                "starts_at": job.cron_starts_at,
                "ends_at": job.cron_ends_at,
                "next_run_at": job.cron_next_run_at,
                "input": job.input,
                "created_at": job.created_at,
            }})))
        }
    }
}

pub async fn list(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    params: web::Query<PaginationParams>,
    filters: JobFilters,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let limit = params.effective_limit();
    let cursor = params.decode_cursor();
    let items = db::jobs::list(&mut db, cursor.as_deref(), limit + 1, &filters.0).await?;

    let has_more = items.len() as i64 > limit;
    let items: Vec<_> = items.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        items.last().map(|j| encode_cursor(&j.job_id))
    } else {
        None
    };
    let data: Vec<serde_json::Value> = items.into_iter().map(|j| job_summary(&j)).collect();

    Ok(HttpResponse::Ok().json(PaginatedResponse {
        data,
        cursor: next_cursor,
    }))
}

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
    let job_id = path.into_inner();
    let job = db::jobs::get(&mut db, &job_id)
        .await?
        .ok_or(AppError::JobNotFound(job_id))?;
    let exec = db::executions::get_for_job(&mut db, &job.job_id).await?;
    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": job_response(&job, exec.as_ref()) })))
}

// `drop(db)` below is not about running a destructor — it marks the point
// where the DbContext hands the `&mut tx` borrow back so the transaction
// can be used (and committed) directly. Keep it: NLL would end the borrow
// anyway, but the explicit release is what the surrounding comments refer to.
#[allow(clippy::drop_non_drop)]
pub async fn update(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
    body: web::Json<UpdateJob>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let job_id = path.into_inner();
    let old_job = {
        let mut db = DbContext::new(&mut conn, prefix);
        db::jobs::get(&mut db, &job_id)
            .await?
            .ok_or_else(|| AppError::JobNotFound(job_id.clone()))?
    };

    if old_job.trigger_type != "CRON" {
        return Err(AppError::JobNotUpdatable(
            "Only CRON jobs can be updated".into(),
        ));
    }
    if old_job.status != "ACTIVE" {
        return Err(AppError::JobNotUpdatable("Job is not active".into()));
    }
    // INTERNAL jobs are invokr-managed (today: the dogfooded reaper). Changing
    // the schedule or pushing ends_at into the past would break monitoring for
    // this workspace, with nothing left to re-provision the job afterwards.
    if EndpointType::from_str_val(&old_job.endpoint_type) == Some(EndpointType::INTERNAL) {
        return Err(AppError::JobNotUpdatable(
            "Internal invokr jobs cannot be modified through the API".into(),
        ));
    }

    let cron_expr =
        match body.cron.clone() {
            Some(c) => c,
            None => PgCronExpr::try_from(old_job.cron_expression.clone().ok_or_else(|| {
                AppError::InvalidCron("Existing job has no cron expression".into())
            })?)?,
        };
    let tz_str = body
        .timezone
        .as_deref()
        .unwrap_or(old_job.cron_timezone.as_deref().unwrap_or("UTC"));

    let schedule = cron_expr.to_schedule();
    let tz: chrono_tz::Tz = tz_str
        .parse()
        .map_err(|_| AppError::InvalidRequest(format!("Invalid timezone: {}", tz_str)))?;

    let next_run = compute_next_cron(&schedule, &tz, Utc::now())
        .ok_or_else(|| AppError::InvalidCron("No upcoming run".into()))?;

    let mut new_job = old_job.clone();
    new_job.cron_expression = Some(cron_expr.as_str().to_string());
    new_job.cron_timezone = Some(tz_str.to_string());
    new_job.cron_next_run_at = Some(next_run);
    new_job.version = old_job.version + 1;
    if let Some(ref input) = body.input {
        new_job.input = Some(input.clone());
    }
    if let Some(starts_at) = body.starts_at {
        new_job.cron_starts_at = Some(starts_at);
    }
    new_job.cron_ends_at = body.ends_at.or(old_job.cron_ends_at);

    // Drop the scoped connection before starting a transaction
    drop(conn);

    let mut tx = invokr_common::db::scoped::scoped_transaction(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut tx, prefix);

    let created = db::jobs::retire_and_replace(&mut db, &job_id, &new_job).await?;

    drop(db);

    // Unschedule the old pg_cron job and register the new one on the same tx as
    // the version flip, so the retire/replace and the schedule swap commit (or
    // roll back) atomically.
    db::jobs::unregister_pg_cron_conn(&mut tx, &ws.0.schema_name, &job_id).await?;
    db::jobs::register_pg_cron_conn(
        &mut tx,
        prefix,
        &ws.0.schema_name,
        &created.job_id,
        cron_expr.as_str(),
    )
    .await?;

    tx.commit().await.map_err(AppError::from)?;

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": {
        "job_id": created.job_id,
        "endpoint": created.endpoint,
        "endpoint_type": created.endpoint_type,
        "trigger": created.trigger_type,
        "status": created.status,
        "version": created.version,
        "previous_version_id": created.previous_version_id,
        "cron": created.cron_expression,
        "timezone": created.cron_timezone,
        "next_run_at": created.cron_next_run_at,
        "input": created.input,
        "created_at": created.created_at,
    }})))
}

// `drop(db)` below is not about running a destructor — it marks the point
// where the DbContext hands the `&mut tx` borrow back so the transaction
// can be used (and committed) directly. Keep it: NLL would end the borrow
// anyway, but the explicit release is what the surrounding comments refer to.
#[allow(clippy::drop_non_drop)]
pub async fn cancel(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut tx = invokr_common::db::scoped::scoped_transaction(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut tx, prefix);
    let job_id = path.into_inner();
    let job = db::jobs::get(&mut db, &job_id)
        .await?
        .ok_or_else(|| AppError::JobNotFound(job_id.clone()))?;

    if job.status == "RETIRED" {
        return Err(AppError::Conflict("Job is already retired".into()));
    }
    // INTERNAL jobs are invokr-managed (today: the dogfooded reaper). Cancelling
    // one would stop the reaper from sweeping this workspace, with no surviving
    // bootstrap path to bring it back.
    if EndpointType::from_str_val(&job.endpoint_type) == Some(EndpointType::INTERNAL) {
        return Err(AppError::Conflict(
            "Internal invokr jobs cannot be cancelled through the API".into(),
        ));
    }

    let cancelled_executions = if job.trigger_type != "CRON" {
        db::executions::cancel_pending_for_job(&mut db, &job_id).await?
    } else {
        vec![]
    };

    let cancelled = db::jobs::cancel(&mut db, &job_id)
        .await?
        .ok_or_else(|| AppError::Conflict("Job could not be cancelled".into()))?;

    drop(db);

    // Best-effort DELETE notification for any WAITING executions that were cancelled
    for c in &cancelled_executions {
        if c.previous_status == "WAITING" {
            if let Some(poll_url) = c.poll_url.clone() {
                let endpoint_name = c.endpoint.clone();
                let pool = state.pool.clone();
                let prefix = state.config.db.table_prefix.clone();
                let schema = ws.0.schema_name.clone();
                let key = state.config.crypto.encryption_key.clone();
                let client = reqwest::Client::new();
                let exec_id = c.execution_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::handlers::executions::send_cancel_delete(
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
    }

    if job.trigger_type == "CRON" {
        db::jobs::unregister_pg_cron_conn(&mut tx, &ws.0.schema_name, &job_id).await?;
    }

    tx.commit().await.map_err(AppError::from)?;

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": job_summary(&cancelled) })))
}

pub async fn status(
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
    let job_id = path.into_inner();
    let job = db::jobs::get(&mut db, &job_id)
        .await?
        .ok_or_else(|| AppError::JobNotFound(job_id.clone()))?;

    let execs = db::executions::list_for_job(&mut db, &job_id, None, 200).await?;

    let active = execs
        .iter()
        .filter(|e| {
            matches!(
                e.status.as_str(),
                "PENDING" | "QUEUED" | "RUNNING" | "RETRYING"
            )
        })
        .count();
    let succeeded = execs.iter().filter(|e| e.status == "SUCCESS").count();
    let failed = execs.iter().filter(|e| e.status == "FAILED").count();

    let health = if execs.is_empty() {
        "IDLE"
    } else if failed > succeeded {
        "FAILING"
    } else if failed > 0 {
        "DEGRADED"
    } else {
        "HEALTHY"
    };

    let last_exec = execs
        .iter()
        .find(|e| e.status == "SUCCESS" || e.status == "FAILED");

    let latest_exec = execs.first();

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": {
        "job_id": job.job_id,
        "job_status": job.status,
        "latest_execution": latest_exec.map(|e| serde_json::json!({
            "execution_id": e.execution_id,
            "status": e.status,
            "attempt_count": e.attempt_count,
            "max_attempts": e.max_attempts,
            "started_at": e.started_at,
            "completed_at": e.completed_at,
        })),
        "endpoint": job.endpoint,
        "endpoint_type": job.endpoint_type,
        "trigger": job.trigger_type,
        "health": health,
        "version": job.version,
        "last_execution": last_exec.map(|e| serde_json::json!({
            "execution_id": e.execution_id,
            "status": e.status,
            "started_at": e.started_at,
            "completed_at": e.completed_at,
            "attempt_number": e.attempt_count,
        })),
        "active_executions": {
            "pending": execs.iter().filter(|e| e.status == "PENDING" || e.status == "QUEUED").count(),
            "running": execs.iter().filter(|e| e.status == "RUNNING" || e.status == "RETRYING").count(),
            "total": active,
        },
        "cron": if job.trigger_type == "CRON" { Some(serde_json::json!({
            "expression": job.cron_expression,
            "next_run_at": job.cron_next_run_at,
            "last_tick_at": job.cron_last_tick_at,
        })) } else { None },
    }})))
}

pub async fn versions(
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
    let job_id = path.into_inner();
    let _ = db::jobs::get(&mut db, &job_id)
        .await?
        .ok_or_else(|| AppError::JobNotFound(job_id.clone()))?;

    let versions = db::jobs::get_versions(&mut db, &job_id).await?;
    let items: Vec<serde_json::Value> = versions.into_iter().map(|j| job_summary(&j)).collect();

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": items })))
}

pub async fn list_executions(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let job_id = path.into_inner();
    let _ = db::jobs::get(&mut db, &job_id)
        .await?
        .ok_or_else(|| AppError::JobNotFound(job_id.clone()))?;

    let limit = params.effective_limit();
    let cursor = params.decode_cursor();
    let items =
        db::executions::list_for_job(&mut db, &job_id, cursor.as_deref(), limit + 1).await?;

    let has_more = items.len() as i64 > limit;
    let items: Vec<_> = items.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        items.last().map(|e| encode_cursor(&e.execution_id))
    } else {
        None
    };

    let data: Vec<serde_json::Value> = items
        .into_iter()
        .map(|e| {
            serde_json::json!({
                "execution_id": e.execution_id,
                "job_id": e.job_id,
                "status": e.status,
                "attempt_count": e.attempt_count,
                "max_attempts": e.max_attempts,
                "input": e.input,
                "output": e.output,
                "run_at": e.run_at,
                "started_at": e.started_at,
                "completed_at": e.completed_at,
                "created_at": e.created_at,
            })
        })
        .collect();

    Ok(HttpResponse::Ok().json(PaginatedResponse {
        data,
        cursor: next_cursor,
    }))
}

fn validate_input(input: &serde_json::Value, schema: &serde_json::Value) -> Result<(), AppError> {
    let compiled = jsonschema::JSONSchema::compile(schema)
        .map_err(|e| AppError::InvalidSchema(format!("{}", e)))?;

    if let Err(errors) = compiled.validate(input) {
        let msgs: Vec<String> = errors.map(|e| e.to_string()).collect();
        return Err(AppError::InputValidationFailed(msgs.join("; ")));
    }
    Ok(())
}

fn compute_next_cron(
    schedule: &cron::Schedule,
    tz: &chrono_tz::Tz,
    after: chrono::DateTime<Utc>,
) -> Option<chrono::DateTime<Utc>> {
    let after_tz = after.with_timezone(tz);
    schedule
        .after(&after_tz)
        .next()
        .map(|dt| dt.with_timezone(&Utc))
}

fn job_response(
    job: &invokr_common::models::Job,
    exec: Option<&invokr_common::models::Execution>,
) -> serde_json::Value {
    let mut v = job_summary(job);
    if let Some(e) = exec {
        v.as_object_mut().unwrap().insert(
            "execution".into(),
            serde_json::json!({
                "execution_id": e.execution_id,
                "status": e.status,
                "created_at": e.created_at,
            }),
        );
    }
    v
}

fn job_summary(job: &invokr_common::models::Job) -> serde_json::Value {
    serde_json::json!({
        "job_id": job.job_id,
        "endpoint": job.endpoint,
        "endpoint_type": job.endpoint_type,
        "trigger": job.trigger_type,
        "status": job.status,
        "version": job.version,
        "idempotency_key": job.idempotency_key,
        "input": job.input,
        "run_at": job.run_at,
        "cron": job.cron_expression,
        "timezone": job.cron_timezone,
        "starts_at": job.cron_starts_at,
        "ends_at": job.cron_ends_at,
        "next_run_at": job.cron_next_run_at,
        "created_at": job.created_at,
    })
}
