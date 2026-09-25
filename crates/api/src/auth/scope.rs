//! Request scoping.
//!
//! Invokr has no authorization layer, so there is no *tenancy* to model here:
//! every authenticated caller reaches every org and workspace. The only
//! distinction this scope draws is whether a route requires a credential at
//! all — infrastructure endpoints and the login callback must not, or the
//! callback would be redirected into the login it is trying to complete.
//!
//! When an authorization layer arrives, this is where an org/workspace realm
//! would be added, and it would then key both the credential cache and any
//! policy lookup.

use std::fmt::Display;

use authn_kit::{AuthRequest, AuthScope, ScopeResolver};

/// The browser session cookie's name.
///
/// **Single source of truth, deliberately.** [`AuthScope::session_cookie_name`]
/// is what `SessionAuthenticator` *reads*, while `CookieSettings::session` is
/// what `LoginFlow` *writes*. If those two ever disagree, login appears to
/// succeed and every subsequent request is anonymous — with no error anywhere.
pub const SESSION_COOKIE: &str = "invokr_session";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InvokrScope {
    /// Requires no credential: health, metrics, the OIDC callback, and the
    /// dashboard's static assets, which the browser fetches before any login.
    Public,
    /// Everything else.
    Protected,
}

impl Display for InvokrScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Public => f.write_str("public"),
            Self::Protected => f.write_str("protected"),
        }
    }
}

impl AuthScope for InvokrScope {
    fn is_public(&self) -> bool {
        matches!(self, Self::Public)
    }

    /// One cookie for the whole service, on both variants — the name must not
    /// vary with the scope a request happens to resolve to, or a session
    /// established on one route would be invisible on another.
    fn session_cookie_name(&self) -> String {
        SESSION_COOKIE.to_string()
    }
}

/// Decides whether a request needs a credential.
pub struct InvokrScopes {
    path_prefix: String,
    dashboard_prefix: String,
}

impl InvokrScopes {
    pub fn new(path_prefix: impl Into<String>, dashboard_prefix: impl Into<String>) -> Self {
        Self {
            path_prefix: path_prefix.into(),
            dashboard_prefix: dashboard_prefix.into(),
        }
    }

    /// The OIDC callback path, derived from the API prefix — the same
    /// construction `setup::build` uses for the registered redirect URI.
    pub fn callback_path(path_prefix: &str) -> String {
        format!("{path_prefix}/oidc/login")
    }
}

impl ScopeResolver for InvokrScopes {
    type Scope = InvokrScope;

    fn resolve(&self, request: &AuthRequest) -> InvokrScope {
        let path = request.path();
        let api = &self.path_prefix;

        // `/metrics` is mounted *inside* the API prefix (see `router.rs`), so a
        // bare "/metrics" test would never match a prefixed deployment and
        // Prometheus would silently start collecting 401s instead of samples.
        let public = path == format!("{api}/health")
            || path == format!("{api}/metrics")
            || path == InvokrScopes::callback_path(api)
            // Served before the user can possibly be authenticated.
            || path.starts_with(&format!("{}/pkg/", self.dashboard_prefix));

        if public {
            InvokrScope::Public
        } else {
            InvokrScope::Protected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &str) -> AuthRequest {
        AuthRequest::builder().method("GET").path(path).build()
    }

    #[test]
    fn health_and_metrics_are_public_under_a_path_prefix() {
        let scopes = InvokrScopes::new("/invokr", "/dashboard");

        assert!(scopes.resolve(&request("/invokr/health")).is_public());
        // `/metrics` is mounted *inside* the API prefix. Testing a bare
        // "/metrics" would pass against an unprefixed deployment and leave a
        // prefixed one serving 401s to Prometheus, which reports as missing
        // data rather than an error.
        assert!(scopes.resolve(&request("/invokr/metrics")).is_public());
    }

    #[test]
    fn health_and_metrics_are_public_without_a_path_prefix() {
        let scopes = InvokrScopes::new("", "");

        assert!(scopes.resolve(&request("/health")).is_public());
        assert!(scopes.resolve(&request("/metrics")).is_public());
    }

    #[test]
    fn the_oidc_callback_is_public() {
        // If the callback required authentication, the session authenticator
        // would redirect it into the login flow it is trying to complete — an
        // infinite loop that only appears once a real provider is configured.
        let scopes = InvokrScopes::new("/invokr", "/dashboard");
        assert!(scopes.resolve(&request("/invokr/oidc/login")).is_public());
    }

    #[test]
    fn the_callback_path_matches_the_registered_redirect_uri() {
        // `setup::build` derives the redirect URI it registers with the
        // provider from the same prefix. If these two ever diverge, the
        // provider redirects to a path that requires authentication.
        assert_eq!(InvokrScopes::callback_path("/invokr"), "/invokr/oidc/login");
        assert_eq!(InvokrScopes::callback_path(""), "/oidc/login");
    }

    #[test]
    fn dashboard_assets_are_public() {
        // Fetched by the browser before any login can have happened.
        let scopes = InvokrScopes::new("/invokr", "/dashboard");
        assert!(scopes
            .resolve(&request("/dashboard/pkg/invokr_dashboard.js"))
            .is_public());
        assert!(scopes
            .resolve(&request("/dashboard/pkg/invokr_dashboard_bg.wasm"))
            .is_public());
    }

    #[test]
    fn api_and_dashboard_routes_are_protected() {
        let scopes = InvokrScopes::new("/invokr", "/dashboard");

        for path in [
            "/invokr/v1/jobs",
            "/invokr/v1/orgs",
            "/invokr/v1/endpoints",
            "/dashboard/jobs",
            "/dashboard/",
        ] {
            assert_eq!(
                scopes.resolve(&request(path)),
                InvokrScope::Protected,
                "{path} must require a credential"
            );
        }
    }

    #[test]
    fn a_path_merely_containing_health_is_not_public() {
        // Substring matching here would expose any route whose name happens to
        // contain an infrastructure path.
        let scopes = InvokrScopes::new("/invokr", "/dashboard");
        assert_eq!(
            scopes.resolve(&request("/invokr/v1/jobs/health-check")),
            InvokrScope::Protected
        );
        assert_eq!(
            scopes.resolve(&request("/invokr/v1/endpoints/metrics")),
            InvokrScope::Protected
        );
    }

    #[test]
    fn the_session_cookie_name_does_not_vary_by_scope() {
        // `SessionAuthenticator` reads the cookie named by the *scope*, while
        // `LoginFlow` writes the one named in `CookieSettings`. If the name
        // varied per scope, a session established on one route would be
        // invisible on another — login would appear to succeed and every
        // request stay anonymous.
        assert_eq!(InvokrScope::Public.session_cookie_name(), SESSION_COOKIE);
        assert_eq!(InvokrScope::Protected.session_cookie_name(), SESSION_COOKIE);
    }
}
