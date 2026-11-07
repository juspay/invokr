#![deny(unused_crate_dependencies)]
use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
struct AppState {
    request_count: Arc<AtomicU64>,
}

#[derive(Serialize)]
struct EchoResponse {
    method: String,
    path: String,
    headers: std::collections::HashMap<String, String>,
    body: serde_json::Value,
}

#[derive(Deserialize)]
struct DelayQuery {
    ms: Option<u64>,
}

#[derive(Deserialize)]
struct FailQuery {
    code: Option<u16>,
    message: Option<String>,
}

/// Always returns 200 OK with the received body echoed back.
async fn success(req: HttpRequest, body: web::Json<serde_json::Value>) -> HttpResponse {
    let headers: std::collections::HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    HttpResponse::Ok().json(EchoResponse {
        method: req.method().to_string(),
        path: req.path().to_string(),
        headers,
        body: body.into_inner(),
    })
}

/// Always returns an error. Use ?code=500&message=oops to customize.
async fn fail(query: web::Query<FailQuery>) -> HttpResponse {
    let code = query.code.unwrap_or(500);
    let message = query
        .message
        .clone()
        .unwrap_or_else(|| "Simulated failure".into());

    HttpResponse::build(
        actix_web::http::StatusCode::from_u16(code)
            .unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR),
    )
    .json(serde_json::json!({
        "error": {
            "code": format!("MOCK_{}", code),
            "message": message,
        }
    }))
}

/// Responds after a configurable delay. Use ?ms=2000 for 2-second delay.
async fn slow(query: web::Query<DelayQuery>, body: web::Json<serde_json::Value>) -> HttpResponse {
    let delay = query.ms.unwrap_or(3000);
    tokio::time::sleep(Duration::from_millis(delay)).await;
    HttpResponse::Ok().json(serde_json::json!({
        "delayed_ms": delay,
        "body": body.into_inner(),
    }))
}

/// Echoes the full request back (method, path, headers, body).
async fn echo(req: HttpRequest, body: web::Json<serde_json::Value>) -> HttpResponse {
    let headers: std::collections::HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    HttpResponse::Ok().json(EchoResponse {
        method: req.method().to_string(),
        path: req.path().to_string(),
        headers,
        body: body.into_inner(),
    })
}

/// Fails on the first N-1 requests and succeeds on the Nth.
/// Use ?succeed_after=3 to fail twice then succeed.
async fn flaky(
    state: web::Data<AppState>,
    query: web::Query<std::collections::HashMap<String, String>>,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let succeed_after: u64 = query
        .get("succeed_after")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    let count = state.request_count.fetch_add(1, Ordering::SeqCst) + 1;

    if count >= succeed_after {
        HttpResponse::Ok().json(serde_json::json!({
            "attempt": count,
            "status": "success",
            "body": body.into_inner(),
        }))
    } else {
        HttpResponse::InternalServerError().json(serde_json::json!({
            "attempt": count,
            "status": "failed",
            "error": format!("Failing until attempt {}", succeed_after),
        }))
    }
}

/// Reset the flaky endpoint counter.
async fn reset_flaky(state: web::Data<AppState>) -> HttpResponse {
    state.request_count.store(0, Ordering::SeqCst);
    HttpResponse::Ok().json(serde_json::json!({ "reset": true }))
}

/// Health check.
async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({ "status": "ok" }))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    let port: u16 = std::env::var("MOCK_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9999);

    let state = AppState {
        request_count: Arc::new(AtomicU64::new(0)),
    };

    tracing::info!("Mock server starting on 0.0.0.0:{}", port);

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .route("/health", web::get().to(health))
            .route("/success", web::post().to(success))
            .route("/fail", web::post().to(fail))
            .route("/fail", web::get().to(fail))
            .route("/slow", web::post().to(slow))
            .route("/echo", web::post().to(echo))
            .route("/echo", web::put().to(echo))
            .route("/flaky", web::post().to(flaky))
            .route("/flaky/reset", web::post().to(reset_flaky))
    })
    .bind(format!("0.0.0.0:{}", port))?
    .run()
    .await
}
