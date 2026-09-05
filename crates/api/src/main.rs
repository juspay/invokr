// Leptos SSR renders the dashboard's deeply-nested `view!` trees, whose generated
// generic types exceed rustc's default recursion limit (128) when this binary
// monomorphizes the server-side render. Raise it so codegen can compute the
// layout of the JobsTab view (multi-select + date-range filters add depth).
#![recursion_limit = "512"]

use actix_cors::Cors;
use actix_web::{error::InternalError, web, App, HttpResponse, HttpServer};
use invokr_common::config::{AppConfig, MigrationMode, ServerMode};
use tracing_subscriber::EnvFilter;

/// Turn actix's default plaintext body-deserialization errors (e.g.
/// `Json deserialize error: premature end of input at line 1 column 75`)
/// into the structured `{ "error": { code, message } }` shape the rest of
/// the API uses, with a 400 status.
fn json_error_handler(
    err: actix_web::error::JsonPayloadError,
    req: &actix_web::HttpRequest,
) -> actix_web::Error {
    // The `RequestId` middleware stamps every request with an `x-request-id`
    // header; surface it so malformed-body errors are traceable like any other.
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| serde_json::Value::String(s.to_owned()))
        .unwrap_or(serde_json::Value::Null);
    let message = format!("Malformed JSON request body: {err}");
    let response = HttpResponse::BadRequest().json(serde_json::json!({
        "error": {
            "code": "INVALID_REQUEST",
            "message": message,
            "request_id": request_id,
        }
    }));
    InternalError::from_response(err, response).into()
}

mod dashboard;
mod extractors;
mod handlers;
mod middleware;
mod router;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("invokr=debug".parse()?))
        .json()
        .init();

    // `app migrate` applies migrations and exits, so the same image can run as a
    // pre-upgrade Job. Exec-form ENTRYPOINT means Kubernetes `args: ["migrate"]`
    // appends cleanly, with no separate migration image to keep in lockstep.
    let migrate_only = std::env::args().nth(1).as_deref() == Some("migrate");

    let config = AppConfig::from_env().await?;
    let pool = invokr_common::db::connect_pool(&config.db.url, config.db.pool_size).await?;

    if migrate_only {
        match config.db.migration_mode {
            MigrationMode::Run => invokr_common::migrate::run(&pool).await?,
            MigrationMode::DryRun => invokr_common::migrate::dry_run(&pool).await?,
            MigrationMode::None => {
                tracing::info!("INVOKR_DB_MIGRATION_MODE is none; nothing to do")
            }
        }
        return Ok(());
    }

    let metrics_handle = invokr_common::metrics::install_recorder();

    let listen_addr = config.server.listen_addr.clone();
    let path_prefix = config.server.path_prefix.clone();
    let mode = config.server.mode.clone();
    let dashboard_prefix = config.server.dashboard_prefix.clone();
    let dashboard_dist_dir = config.server.dashboard_dist_dir.clone();

    let app_state = router::AppState {
        pool: pool.clone(),
        config: config.clone(),
        metrics_handle,
    };

    tracing::info!("Server mode: {:?}", mode);
    tracing::info!("API server listening on {}", listen_addr);
    if !path_prefix.is_empty() {
        tracing::info!("API path prefix: {}", path_prefix);
    }

    // Build dashboard config if needed
    let dashboard_config = if mode != ServerMode::Api {
        tracing::info!(
            "Dashboard dist dir: {}, prefix: {:?}",
            dashboard_dist_dir,
            dashboard_prefix
        );
        Some(invokr_dashboard::config::DashboardConfig {
            api_base_url: String::new(), // same-origin; server functions handle routing
            api_prefix: path_prefix.clone(),
            dashboard_prefix: dashboard_prefix.clone(),
            api_key: config.server.api_key.clone(),
        })
    } else {
        None
    };

    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);

        let mut app = App::new()
            .app_data(web::Data::new(app_state.clone()))
            .app_data(web::JsonConfig::default().error_handler(json_error_handler))
            .wrap(cors)
            .wrap(actix_web::middleware::Logger::default())
            .wrap(crate::middleware::RequestId);

        // Register API routes (specific paths first)
        if mode == ServerMode::Api || mode == ServerMode::Both {
            app = app.configure(router::configure(&path_prefix, &mode, &dashboard_prefix));
        }

        // Register dashboard routes (catch-all last)
        if let Some(ref dc) = dashboard_config {
            app = app.configure(dashboard::configure(
                &dashboard_prefix,
                dc.clone(),
                &dashboard_dist_dir,
            ));
        }

        app
    })
    .bind(&listen_addr)?
    .run()
    .await?;

    Ok(())
}
