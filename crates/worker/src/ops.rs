use std::sync::Arc;
use std::time::Duration;

use actix_web::{dev::Server, web, App, HttpResponse, HttpServer};
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;

use crate::health::WorkerHealth;

#[derive(Clone)]
pub struct OpsState {
    pub pool: PgPool,
    pub health: Arc<WorkerHealth>,
    pub metrics: PrometheusHandle,
    pub db_probe_timeout: Duration,
}

pub fn ready_response(
    db: Result<(), String>,
    poller_ok: bool,
    last_tick_ago_ms: u64,
) -> HttpResponse {
    let ready = db.is_ok() && poller_ok;

    let body = serde_json::json!({
        "status": if ready { "ready" } else { "not_ready" },
        "db": match &db {
            Ok(()) => "ok",
            Err(e) => e.as_str(),
        },
        "poller": if poller_ok { "ok" } else { "stale" },
        "last_tick_ago_ms": last_tick_ago_ms,
    });

    if ready {
        HttpResponse::Ok().json(body)
    } else {
        HttpResponse::ServiceUnavailable().json(body)
    }
}

pub async fn probe_db(pool: &PgPool, timeout: Duration) -> Result<(), String> {
    match tokio::time::timeout(timeout, sqlx::query("SELECT 1").execute(pool)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("probe timed out after {timeout:?}")),
    }
}

async fn health_handler() -> HttpResponse {
    HttpResponse::Ok().body("OK")
}

async fn ready_handler(state: web::Data<OpsState>) -> HttpResponse {
    let db = probe_db(&state.pool, state.db_probe_timeout).await;
    ready_response(
        db,
        state.health.poller_ok(),
        state.health.last_tick_ago().as_millis() as u64,
    )
}

async fn metrics_handler(state: web::Data<OpsState>) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4")
        .body(state.metrics.render())
}

pub fn routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health_handler))
        .route("/ready", web::get().to(ready_handler))
        .route("/metrics", web::get().to(metrics_handler));
}

pub fn ops_server(port: u16, workers: usize, state: OpsState) -> std::io::Result<Server> {
    Ok(HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .configure(routes)
    })
    .workers(workers)
    .disable_signals()
    .bind(("0.0.0.0", port))?
    .run())
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{body::to_bytes, http::StatusCode, test as actix_test};
    use metrics_exporter_prometheus::PrometheusBuilder;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn ready_response_is_200_when_db_and_poller_ok() {
        let resp = ready_response(Ok(()), true, 42);
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn ready_response_is_503_when_db_is_down() {
        let resp = ready_response(Err("connection refused".into()), true, 42);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn ready_response_is_503_when_poller_is_stale() {
        let resp = ready_response(Ok(()), false, 94_000);
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn ready_body_reports_each_check() {
        let resp = ready_response(Err("pool timed out".into()), false, 94_000);
        let body = to_bytes(resp.into_body()).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["db"], "pool timed out");
        assert_eq!(json["poller"], "stale");
        assert_eq!(json["last_tick_ago_ms"], 94_000);
    }

    /// A pool aimed at a closed port. `connect_lazy` means construction does not
    /// block, so the failure surfaces in the probe where readiness can report it.
    fn unreachable_pool() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(500))
            .connect_lazy("postgres://invokr:invokr@127.0.0.1:1/invokr_db")
            .unwrap()
    }

    fn test_state() -> OpsState {
        OpsState {
            pool: unreachable_pool(),
            health: Arc::new(WorkerHealth::new(
                1,
                Duration::from_millis(200),
                Duration::from_secs(5),
            )),
            // build_recorder, not install_recorder: the global recorder can only
            // be installed once per process and these tests share one.
            metrics: PrometheusBuilder::new().build_recorder().handle(),
            db_probe_timeout: Duration::from_millis(200),
        }
    }

    #[actix_web::test]
    async fn health_endpoint_is_ok_even_when_db_is_down() {
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(test_state()))
                .configure(routes),
        )
        .await;

        let resp = actix_test::call_service(
            &app,
            actix_test::TestRequest::get().uri("/health").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = to_bytes(resp.into_body()).await.unwrap();
        assert_eq!(&body[..], b"OK");
    }

    #[actix_web::test]
    async fn ready_endpoint_is_503_when_db_is_unreachable() {
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(test_state()))
                .configure(routes),
        )
        .await;

        let resp = actix_test::call_service(
            &app,
            actix_test::TestRequest::get().uri("/ready").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn probe_db_reports_the_configured_timeout() {
        let err = probe_db(&unreachable_pool(), Duration::from_millis(50))
            .await
            .expect_err("probe against a closed port should fail");
        assert!(
            err.contains("50ms"),
            "timeout should reflect the configured value, got: {err}"
        );
    }

    #[actix_web::test]
    async fn ops_server_binds_and_is_spawnable() {
        // Port 0 lets the OS pick, so the test can't collide with a running
        // worker. Succeeding here proves the route config assembles under a
        // real bind, not just under `init_service`.
        let server = ops_server(0, 1, test_state()).expect("ops server should bind");
        let handle = server.handle();
        tokio::spawn(server);
        handle.stop(false).await;
    }

    #[actix_web::test]
    async fn metrics_endpoint_renders_prometheus_output() {
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(test_state()))
                .configure(routes),
        )
        .await;

        let resp = actix_test::call_service(
            &app,
            actix_test::TestRequest::get().uri("/metrics").to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
