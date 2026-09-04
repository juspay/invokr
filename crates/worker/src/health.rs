use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use actix_web::{dev::Server, web, App, HttpResponse, HttpServer};
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;
use tokio::sync::Semaphore;

pub fn stale_after(poll_interval: Duration, floor: Duration) -> Duration {
    std::cmp::max(floor, poll_interval * 10)
}

pub fn poller_ok(last_tick_ago: Duration, stale_after: Duration, available_permits: usize) -> bool {
    last_tick_ago <= stale_after || available_permits == 0
}

pub struct WorkerHealth {
    started: Instant,
    last_tick_ms: AtomicU64,
    semaphore: Arc<Semaphore>,
    stale_after: Duration,
}

impl WorkerHealth {
    pub fn new(max_concurrent: usize, poll_interval: Duration, stale_floor: Duration) -> Self {
        Self {
            started: Instant::now(),
            last_tick_ms: AtomicU64::new(0),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            stale_after: stale_after(poll_interval, stale_floor),
        }
    }

    pub fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    pub fn tick(&self) {
        self.last_tick_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    pub fn last_tick_ago(&self) -> Duration {
        Duration::from_millis(
            self.now_ms()
                .saturating_sub(self.last_tick_ms.load(Ordering::Relaxed)),
        )
    }

    pub fn poller_ok(&self) -> bool {
        poller_ok(
            self.last_tick_ago(),
            self.stale_after,
            self.semaphore.available_permits(),
        )
    }
}

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

    const FRESH: Duration = Duration::from_millis(50);
    const THRESHOLD: Duration = Duration::from_secs(5);

    #[test]
    fn stale_after_is_ten_poll_intervals() {
        assert_eq!(
            stale_after(Duration::from_millis(2000), Duration::from_secs(5)),
            Duration::from_secs(20)
        );
    }

    #[test]
    fn stale_after_uses_the_configured_floor() {
        // The 200ms poll interval would otherwise yield a 2s threshold.
        assert_eq!(
            stale_after(Duration::from_millis(200), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn poller_ok_when_tick_is_fresh() {
        assert!(poller_ok(FRESH, THRESHOLD, 50));
    }

    #[test]
    fn poller_not_ok_when_tick_is_stale() {
        assert!(!poller_ok(Duration::from_secs(90), THRESHOLD, 50));
    }

    #[test]
    fn poller_ok_when_stale_but_at_capacity() {
        // Saturated, not wedged: every permit is held by an in-flight dispatch.
        assert!(poller_ok(Duration::from_secs(90), THRESHOLD, 0));
    }

    #[test]
    fn new_health_starts_fresh() {
        let health = WorkerHealth::new(1, Duration::from_millis(200), Duration::from_secs(5));
        assert!(health.last_tick_ago() < Duration::from_secs(5));
        assert!(health.poller_ok());
    }

    #[actix_web::test]
    async fn health_is_ok_when_stale_but_every_permit_is_held() {
        let health = WorkerHealth::new(2, Duration::from_millis(1), Duration::from_secs(5));
        // The floor is 5s, so force staleness rather than sleeping it out.
        health.last_tick_ms.store(0, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(10));

        let _held = health.semaphore().acquire_many_owned(2).await.unwrap();
        assert_eq!(health.semaphore().available_permits(), 0);
        assert!(health.poller_ok());
    }

    #[test]
    fn tick_refreshes_last_tick() {
        let health = WorkerHealth::new(1, Duration::from_millis(200), Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
        let before = health.last_tick_ago();
        health.tick();
        assert!(health.last_tick_ago() < before);
    }

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
