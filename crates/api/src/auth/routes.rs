//! The OIDC callback and logout routes.
//!
//! `LoginFlow` returns *data* — a location and a list of cookie directives —
//! and this module is the only place that turns those into an actix response.
//! The protocol logic stays testable without a web framework.

use std::sync::Arc;

use actix_web::{cookie::Cookie, web, HttpRequest, HttpResponse};
use authn_kit::{
    oidc::{CallbackParams, LoginFlow},
    AuthError,
};

use crate::auth::scope::SESSION_COOKIE;

/// What the auth routes need at request time.
#[derive(Clone)]
pub struct AuthRoutesState {
    /// `None` under `INVOKR_AUTH_MODE=disabled` — there is nothing to log in to.
    pub login: Option<Arc<LoginFlow>>,
    pub path_prefix: String,
}

impl AuthRoutesState {
    pub fn disabled(path_prefix: &str) -> Self {
        Self {
            login: None,
            path_prefix: path_prefix.to_string(),
        }
    }

    pub fn enabled(path_prefix: &str, login: Arc<LoginFlow>) -> Self {
        Self {
            login: Some(login),
            path_prefix: path_prefix.to_string(),
        }
    }
}

/// `GET {prefix}/oidc/login` — the provider's callback.
///
/// Registered at exactly the redirect URI `setup::build` derives, and treated as
/// a public scope: authenticating it would redirect the callback into the login
/// it is completing.
async fn callback(
    request: HttpRequest,
    state: web::Data<AuthRoutesState>,
) -> Result<HttpResponse, actix_web::Error> {
    let Some(login) = &state.login else {
        return Ok(HttpResponse::NotFound().json(serde_json::json!({
            "error": {
                "code": "AUTH_DISABLED",
                "message": "Authentication is disabled; there is no login flow"
            }
        })));
    };

    let params = CallbackParams::from_query(request.query_string())
        .map_err(|e| actix_web::error::InternalError::from_response("", render(&e)))?;

    let protection = request
        .cookie(&login.protection_cookie().name)
        .map(|c| c.value().to_string());

    let complete = login
        .complete(params, protection.as_deref())
        .await
        .map_err(|e| actix_web::error::InternalError::from_response("", render(&e)))?;

    let mut response = HttpResponse::Found();
    response.insert_header(("Location", complete.redirect_to.clone()));
    for directive in &complete.cookies {
        response.cookie(Cookie::from(directive));
    }
    Ok(response.finish())
}

/// `GET {prefix}/logout` — clears the session and returns to the root.
async fn logout(state: web::Data<AuthRoutesState>) -> HttpResponse {
    let mut response = HttpResponse::Found();
    response.insert_header(("Location", format!("{}/", state.path_prefix)));

    if let Some(login) = &state.login {
        response.cookie(Cookie::from(&login.session_cookie().clear()));
    } else {
        // Best effort when no flow exists: clear by name and path.
        let mut cookie = Cookie::new(SESSION_COOKIE, "");
        cookie.set_path("/");
        cookie.make_removal();
        response.cookie(cookie);
    }

    response.finish()
}

fn render(error: &AuthError) -> HttpResponse {
    authn_kit::adapters::actix::error_to_response(error)
}

/// Registers the auth routes at their absolute paths.
///
/// Deliberately **not** a `web::scope`. With no `INVOKR_PATH_PREFIX` the scope
/// path is `""`, and an empty actix scope matches every request — it would
/// shadow the entire API and dashboard, which answer `404` from inside it.
/// Registering absolute routes has no such failure mode at either setting.
pub fn configure(path_prefix: &str) -> impl FnOnce(&mut web::ServiceConfig) + 'static {
    let login = format!("{path_prefix}/oidc/login");
    let logout_path = format!("{path_prefix}/logout");

    move |cfg: &mut web::ServiceConfig| {
        cfg.route(&login, web::get().to(callback));
        cfg.route(&logout_path, web::get().to(logout));
    }
}
