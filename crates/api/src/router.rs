use actix_web::{web, HttpResponse};
use invokr_common::config::{AppConfig, ServerMode};
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;

use crate::handlers;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
    pub metrics_handle: PrometheusHandle,
}

impl AppState {
    pub fn prefix(&self) -> &str {
        self.config.db.table_prefix.as_str()
    }
}

pub fn configure(
    prefix: &str,
    mode: &ServerMode,
    dashboard_prefix: &str,
) -> impl FnOnce(&mut web::ServiceConfig) + 'static {
    let prefix = prefix.to_string();
    let mode = mode.clone();
    let dashboard_prefix = dashboard_prefix.to_string();

    move |cfg: &mut web::ServiceConfig| {
        // When a path prefix is configured, redirect `/` appropriately
        if !prefix.is_empty() {
            let redirect_target = if mode == ServerMode::Both && !dashboard_prefix.is_empty() {
                format!("{dashboard_prefix}/")
            } else {
                format!("{prefix}/health")
            };
            cfg.route(
                "/",
                web::get().to(move || {
                    let target = redirect_target.clone();
                    async move {
                        HttpResponse::Found()
                            .insert_header(("Location", target))
                            .finish()
                    }
                }),
            );
        }

        cfg.service(
            web::scope(&prefix)
                .route("/health", web::get().to(health))
                .route("/metrics", web::get().to(metrics_handler))
                // Management routes (no workspace context needed)
                .route("/v1/orgs", web::post().to(handlers::organizations::create))
                .route("/v1/orgs", web::get().to(handlers::organizations::list))
                .route(
                    "/v1/orgs/{org_id}",
                    web::get().to(handlers::organizations::get),
                )
                .route(
                    "/v1/orgs/{org_id}",
                    web::put().to(handlers::organizations::update),
                )
                .route(
                    "/v1/orgs/{org_id}/workspaces",
                    web::post().to(handlers::workspaces::create),
                )
                .route(
                    "/v1/orgs/{org_id}/workspaces",
                    web::get().to(handlers::workspaces::list),
                )
                .route(
                    "/v1/orgs/{org_id}/workspaces/{workspace_id}",
                    web::get().to(handlers::workspaces::get),
                )
                // Callback routes — tenant from URL path, no workspace header needed
                .route(
                    "/v1/callbacks/{org_id}/{workspace_id}/executions/{execution_id}/complete",
                    web::post().to(handlers::callbacks::complete),
                )
                .route(
                    "/v1/callbacks/{org_id}/{workspace_id}/executions/{execution_id}/fail",
                    web::post().to(handlers::callbacks::fail),
                )
                .service(
                    web::scope("/v1")
                        // Payload Specs
                        .route(
                            "/payload-specs",
                            web::post().to(handlers::payload_specs::create),
                        )
                        .route(
                            "/payload-specs",
                            web::get().to(handlers::payload_specs::list),
                        )
                        .route(
                            "/payload-specs/{name}",
                            web::get().to(handlers::payload_specs::get),
                        )
                        .route(
                            "/payload-specs/{name}",
                            web::put().to(handlers::payload_specs::update),
                        )
                        .route(
                            "/payload-specs/{name}",
                            web::delete().to(handlers::payload_specs::delete),
                        )
                        // Configs
                        .route("/configs", web::post().to(handlers::configs::create))
                        .route("/configs", web::get().to(handlers::configs::list))
                        .route("/configs/{name}", web::get().to(handlers::configs::get))
                        .route("/configs/{name}", web::put().to(handlers::configs::update))
                        .route(
                            "/configs/{name}",
                            web::delete().to(handlers::configs::delete),
                        )
                        // Secrets
                        .route("/secrets", web::post().to(handlers::secrets::create))
                        .route("/secrets", web::get().to(handlers::secrets::list))
                        .route("/secrets/{name}", web::get().to(handlers::secrets::get))
                        .route("/secrets/{name}", web::put().to(handlers::secrets::update))
                        .route(
                            "/secrets/{name}",
                            web::delete().to(handlers::secrets::delete),
                        )
                        // Endpoints
                        .route("/endpoints", web::post().to(handlers::endpoints::create))
                        .route("/endpoints", web::get().to(handlers::endpoints::list))
                        .route("/endpoints/{name}", web::get().to(handlers::endpoints::get))
                        .route(
                            "/endpoints/{name}",
                            web::put().to(handlers::endpoints::update),
                        )
                        .route(
                            "/endpoints/{name}",
                            web::delete().to(handlers::endpoints::delete),
                        )
                        // Jobs
                        .route("/jobs", web::post().to(handlers::jobs::create))
                        .route("/jobs", web::get().to(handlers::jobs::list))
                        .route("/jobs/{job_id}", web::get().to(handlers::jobs::get))
                        .route("/jobs/{job_id}", web::put().to(handlers::jobs::update))
                        .route(
                            "/jobs/{job_id}/cancel",
                            web::post().to(handlers::jobs::cancel),
                        )
                        .route(
                            "/jobs/{job_id}/status",
                            web::get().to(handlers::jobs::status),
                        )
                        .route(
                            "/jobs/{job_id}/versions",
                            web::get().to(handlers::jobs::versions),
                        )
                        .route(
                            "/jobs/{job_id}/executions",
                            web::get().to(handlers::jobs::list_executions),
                        )
                        // Executions
                        .route(
                            "/executions/{execution_id}",
                            web::get().to(handlers::executions::get),
                        )
                        .route(
                            "/executions/{execution_id}/cancel",
                            web::post().to(handlers::executions::cancel),
                        )
                        .route(
                            "/executions/{execution_id}/attempts",
                            web::get().to(handlers::executions::list_attempts),
                        )
                        .route(
                            "/executions/{execution_id}/polls",
                            web::get().to(handlers::executions::list_polls),
                        )
                        .route(
                            "/executions/{execution_id}/logs",
                            web::get().to(handlers::executions::list_logs),
                        ),
                ),
        );
    }
}

async fn health() -> &'static str {
    "OK"
}

async fn metrics_handler(state: web::Data<AppState>) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4")
        .body(state.metrics_handle.render())
}
