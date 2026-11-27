#![deny(unused_crate_dependencies)]
// Leptos SSR renders the dashboard's deeply-nested `view!` trees, whose generated
// generic types exceed rustc's default recursion limit (128) when this binary
// monomorphizes the server-side render. Raise it so codegen can compute the
// layout of the JobsTab view (multi-select + date-range filters add depth).
#![recursion_limit = "512"]

use actix_cors::Cors;
use actix_web::{error::InternalError, web, App, HttpResponse, HttpServer};
use invokr_common::config::{AppConfig, ServerMode};
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

mod auth;
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

    let config = AppConfig::from_env().await?;
    let pool = invokr_common::db::connect_pool(&config.db.url, config.db.pool_size).await?;

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

    // Built before the server binds, so a missing variable or an unreachable
    // identity provider aborts at startup with a message naming the problem —
    // rather than surfacing as a 401 on whichever request happens to hit it.
    let auth_env = invokr_common::config::AuthEnv::from_env().await?;
    let authn = auth::build(&auth_env, &path_prefix, &dashboard_prefix)
        .await
        .map_err(|e| anyhow::anyhow!("failed to build authentication: {e}"))?;
    let authn_middleware = authn.middleware;
    let authn_routes = authn.routes;

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
        })
    } else {
        None
    };

    let cors_origins = parse_cors_origins(std::env::var(CORS_ORIGINS_VAR).ok())?;
    match &cors_origins {
        Some(origins) => tracing::info!("CORS allowed origins: {}", origins.join(", ")),
        None => tracing::info!("CORS: any origin (set {CORS_ORIGINS_VAR} to restrict)"),
    }

    HttpServer::new(move || {
        let cors = build_cors(cors_origins.as_deref());

        let mut app = App::new()
            .app_data(web::Data::new(app_state.clone()))
            .app_data(web::Data::new(authn_routes.clone()))
            .app_data(web::JsonConfig::default().error_handler(json_error_handler))
            // Innermost, so CORS and logging still run on an auth failure.
            // In actix the last `.wrap()` runs first on the way in.
            .wrap(authn_middleware.clone())
            .wrap(cors)
            .wrap(actix_web::middleware::Logger::default())
            .wrap(crate::middleware::RequestId);

        // The OIDC callback and logout live under the API prefix, and the
        // callback must match the redirect URI registered with the provider.
        app = app.configure(auth::routes::configure(&path_prefix));

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

const CORS_ORIGINS_VAR: &str = "INVOKR_CORS_ALLOWED_ORIGINS";

/// Reads `INVOKR_CORS_ALLOWED_ORIGINS`: comma-separated browser origins
/// allowed to call the API. `None` (unset, blank or `*`) keeps any origin,
/// which is safe on its own: CORS here never allows credentials and the
/// session cookie is `SameSite=Lax`, so another site can neither send it nor
/// read a signed-in response. Production should still list its origins.
fn parse_cors_origins(raw: Option<String>) -> anyhow::Result<Option<Vec<String>>> {
    let Some(raw) = raw.filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    let origins: Vec<String> = raw
        .split(',')
        .map(|o| o.trim().trim_end_matches('/').to_owned())
        .filter(|o| !o.is_empty())
        .collect();
    if origins.iter().any(|o| o == "*") {
        return Ok(None);
    }
    for origin in &origins {
        // scheme://host[:port] — no path, which is what browsers send.
        let host = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"));
        let looks_like_origin = host.is_some_and(|h| !h.is_empty() && !h.contains('/'))
            && actix_web::http::header::HeaderValue::from_str(origin).is_ok();
        if !looks_like_origin {
            anyhow::bail!(
                "{CORS_ORIGINS_VAR}: '{origin}' is not an origin like https://app.example.com"
            );
        }
    }
    Ok(Some(origins))
}

fn build_cors(origins: Option<&[String]>) -> Cors {
    let cors = Cors::default()
        .allow_any_method()
        .allow_any_header()
        .max_age(3600);
    match origins {
        None => cors.allow_any_origin(),
        Some(list) => list.iter().fold(cors, |cors, o| cors.allowed_origin(o)),
    }
}

#[cfg(test)]
mod cors_tests {
    use super::*;
    // Not `use actix_web::test`: it would shadow the built-in `#[test]`.
    use actix_web::http::header;
    use actix_web::test as atest;

    fn parse(raw: &str) -> Option<Vec<String>> {
        parse_cors_origins(Some(raw.to_string())).unwrap()
    }

    #[test]
    fn unset_blank_or_star_means_any_origin() {
        assert_eq!(parse_cors_origins(None).unwrap(), None);
        assert_eq!(parse(" "), None);
        assert_eq!(parse("*"), None);
    }

    #[test]
    fn parses_a_trimmed_list() {
        assert_eq!(
            parse(" https://a.example , https://b.example/ "),
            Some(vec![
                "https://a.example".to_string(),
                "https://b.example".to_string()
            ])
        );
    }

    #[test]
    fn rejects_values_that_are_not_origins() {
        for bad in [
            "a.example",
            "ftp://a.example",
            "https://a.example/path",
            "https://",
        ] {
            assert!(
                parse_cors_origins(Some(bad.to_string())).is_err(),
                "{bad:?}"
            );
        }
    }

    async fn acao_for(origins: Option<Vec<String>>, origin: &str) -> (Option<String>, bool) {
        let app = atest::init_service(
            App::new()
                .wrap(build_cors(origins.as_deref()))
                .route("/x", web::get().to(HttpResponse::Ok)),
        )
        .await;
        let req = atest::TestRequest::get()
            .uri("/x")
            .insert_header((header::ORIGIN, origin))
            .to_request();
        let res = atest::call_service(&app, req).await;
        let headers = res.headers();
        (
            headers
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .map(|v| v.to_str().unwrap().to_string()),
            headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
        )
    }

    #[actix_web::test]
    async fn default_allows_any_origin_but_never_credentials() {
        let (acao, creds) = acao_for(None, "https://anywhere.example").await;
        assert!(acao.is_some());
        assert!(!creds, "credentials must never be allowed cross-origin");
    }

    #[actix_web::test]
    async fn allowlist_admits_listed_origins_only() {
        let list = Some(vec!["https://dash.example".to_string()]);
        let (acao, creds) = acao_for(list.clone(), "https://dash.example").await;
        assert_eq!(acao.as_deref(), Some("https://dash.example"));
        assert!(!creds);

        let (acao, _) = acao_for(list, "https://evil.example").await;
        assert_eq!(acao, None, "unlisted origin must not be allowed");
    }
}
