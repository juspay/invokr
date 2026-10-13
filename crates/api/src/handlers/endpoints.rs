use crate::extractors::{AuthenticatedRequest, Workspace};
use crate::router::AppState;
use actix_web::{web, HttpResponse};
use invokr_common::{
    db,
    db::DbContext,
    error::AppError,
    models::endpoint::{CreateEndpoint, UpdateEndpoint},
    pagination::{encode_cursor, PaginatedResponse, PaginationParams},
    service::endpoints as endpoint_svc,
};

pub async fn create(
    state: web::Data<AppState>,
    _auth: AuthenticatedRequest,
    ws: Workspace,
    body: web::Json<CreateEndpoint>,
) -> Result<HttpResponse, AppError> {
    endpoint_svc::ensure_user_endpoint_type(&body.endpoint_type)?;

    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);

    endpoint_svc::check_refs(
        &mut db,
        body.payload_spec.as_deref(),
        body.config.as_deref(),
    )
    .await?;

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
    let prefix = state.prefix();
    let mut conn = invokr_common::db::scoped::scoped_connection(&state.pool, &ws.0.schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut conn, prefix);
    let name = path.into_inner();
    endpoint_svc::check_refs(
        &mut db,
        body.payload_spec.as_deref(),
        body.config.as_deref(),
    )
    .await?;

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
