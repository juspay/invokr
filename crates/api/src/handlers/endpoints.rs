use crate::extractors::{AuthenticatedRequest, Workspace};
use crate::router::AppState;
use actix_web::{web, HttpResponse};
use invokr_common::{
    db,
    db::DbContext,
    error::AppError,
    models::endpoint::{CreateEndpoint, EndpointType, UpdateEndpoint},
    pagination::{encode_cursor, PaginatedResponse, PaginationParams},
};

pub async fn create(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    body: web::Json<CreateEndpoint>,
) -> Result<HttpResponse, AppError> {
    // INTERNAL endpoints exist for invokr-driven internal tasks (e.g. the
    if let Err(msg) = invokr_common::models::endpoint::validate_async_block(&body.spec) {
        return Err(AppError::InvalidRequest(msg));
    }
    // dogfooded reaper) and are provisioned at workspace-creation time —
    // never through the public API. Reject them explicitly so the constraint
    // doesn't have to do it with an opaque "violates check" error.
    match EndpointType::from_str_val(&body.endpoint_type) {
        None | Some(EndpointType::INTERNAL) => {
            return Err(AppError::InvalidRequest(format!(
                "Invalid endpoint type: {}. Must be HTTP, KAFKA, or REDIS_STREAM",
                body.endpoint_type
            )));
        }
        _ => {}
    }

    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);

    if let Some(ref ps) = body.payload_spec {
        if db::payload_specs::get(&mut db, ps).await?.is_none() {
            return Err(AppError::InvalidPayloadSpecRef(ps.clone()));
        }
    }
    if let Some(ref cfg) = body.config {
        if db::configs::get(&mut db, cfg).await?.is_none() {
            return Err(AppError::InvalidConfigRef(cfg.clone()));
        }
    }

    let retry_json = body
        .retry_policy
        .as_ref()
        .map(|rp| serde_json::to_value(rp).unwrap());

    let ep = db::endpoints::create(
        &mut db,
        &body.name,
        &body.endpoint_type,
        body.payload_spec.as_deref(),
        body.config.as_deref(),
        &body.spec,
        retry_json.as_ref(),
    )
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
            AppError::Conflict(format!("Endpoint '{}' already exists", body.name))
        }
        _ => AppError::from(e),
    })?;

    Ok(HttpResponse::Created().json(serde_json::json!({ "data": endpoint_to_json(&ep) })))
}

pub async fn list(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    params: web::Query<PaginationParams>,
) -> Result<HttpResponse, AppError> {
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let limit = params.effective_limit();
    let cursor = params.decode_cursor();
    let items = db::endpoints::list(&mut db, cursor.as_deref(), limit + 1).await?;

    let has_more = items.len() as i64 > limit;
    let items: Vec<_> = items.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        items.last().map(|e| encode_cursor(&e.name))
    } else {
        None
    };
    let data: Vec<serde_json::Value> = items.into_iter().map(|e| endpoint_to_json(&e)).collect();

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
    let name = path.into_inner();
    let ep = db::endpoints::get(&mut db, &name)
        .await?
        .ok_or(AppError::EndpointNotFound(name))?;
    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": endpoint_to_json(&ep) })))
}

pub async fn update(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    path: web::Path<String>,
    body: web::Json<UpdateEndpoint>,
) -> Result<HttpResponse, AppError> {
    if let Some(spec) = &body.spec {
        if let Err(msg) = invokr_common::models::endpoint::validate_async_block(spec) {
            return Err(AppError::InvalidRequest(msg));
        }
    }

    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let name = path.into_inner();
    if let Some(ref ps) = body.payload_spec {
        if db::payload_specs::get(&mut db, ps).await?.is_none() {
            return Err(AppError::InvalidPayloadSpecRef(ps.clone()));
        }
    }
    if let Some(ref cfg) = body.config {
        if db::configs::get(&mut db, cfg).await?.is_none() {
            return Err(AppError::InvalidConfigRef(cfg.clone()));
        }
    }

    let retry_json = body
        .retry_policy
        .as_ref()
        .map(|rp| serde_json::to_value(rp).unwrap());

    let ep = db::endpoints::update(
        &mut db,
        &name,
        body.spec.as_ref(),
        body.config.as_deref(),
        body.payload_spec.as_deref(),
        retry_json.as_ref(),
    )
    .await?
    .ok_or(AppError::EndpointNotFound(name))?;

    Ok(HttpResponse::Ok().json(serde_json::json!({ "data": endpoint_to_json(&ep) })))
}

pub async fn delete(
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
    let name = path.into_inner();
    if db::endpoints::has_active_jobs(&mut db, &name).await? {
        return Err(AppError::Conflict(format!(
            "Endpoint '{}' has active jobs",
            name
        )));
    }
    if !db::endpoints::delete(&mut db, &name).await? {
        return Err(AppError::EndpointNotFound(name));
    }
    Ok(HttpResponse::NoContent().finish())
}

fn endpoint_to_json(ep: &invokr_common::models::Endpoint) -> serde_json::Value {
    serde_json::json!({
        "name": ep.name,
        "type": ep.endpoint_type,
        "payload_spec": ep.payload_spec_ref,
        "config": ep.config_ref,
        "spec": ep.spec,
        "retry_policy": ep.retry_policy,
        "created_at": ep.created_at,
        "updated_at": ep.updated_at,
    })
}
