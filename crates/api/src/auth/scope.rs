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
