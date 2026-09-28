//! Browser-session authentication, with the login redirect confined to the
//! dashboard.
//!
//! `SessionAuthenticator` turns *any* credential-less `GET` into a redirect
//! into the login flow. That is right for a browser navigating to the dashboard
//! and wrong for an API call: `curl {prefix}/v1/jobs` with a forgotten token
//! would follow the redirect and receive the identity provider's login page
//! with a `200`, which looks like a successful request returning nonsense
//! rather than a missing credential.
//!
//! This wrapper keeps the redirect for the dashboard and converts it to a `401`
//! for `/v1/*`, where the caller is a program.

use async_trait::async_trait;
use authn_kit::{
    authenticator::AuthProfile, mechanisms::SessionAuthenticator, AuthContext, AuthError,
    Authenticator, Verdict,
};

use crate::auth::principal::InvokrProfile;

pub struct BrowserSessionAuthenticator {
    inner: SessionAuthenticator<InvokrProfile>,
    /// Requests under `{api_prefix}/v1/` are programs, not browsers.
    api_prefix: String,
}

impl BrowserSessionAuthenticator {
    pub fn new(inner: SessionAuthenticator<InvokrProfile>, api_prefix: impl Into<String>) -> Self {
        Self {
            inner,
            api_prefix: api_prefix.into(),
        }
    }

    fn is_api_call(&self, path: &str) -> bool {
        is_api_call(&self.api_prefix, path)
    }
}

/// Whether `path` is a programmatic API call rather than a browser navigation.
///
/// Free-standing so it is testable without an [`OidcProvider`], which would
/// otherwise require a live identity provider to construct.
///
/// [`OidcProvider`]: authn_kit::oidc::OidcProvider
fn is_api_call(api_prefix: &str, path: &str) -> bool {
    path.starts_with(&format!("{api_prefix}/v1/"))
}

#[async_trait]
impl Authenticator<InvokrProfile> for BrowserSessionAuthenticator {
    fn name(&self) -> &'static str {
        "browser-session"
    }

    async fn authenticate(
        &self,
        ctx: &AuthContext<'_, InvokrProfile>,
    ) -> Result<Verdict<<InvokrProfile as AuthProfile>::User>, AuthError> {
        match self.inner.authenticate(ctx).await {
            // An API caller gets told what is wrong, rather than being sent on a
            // round trip to a login page it cannot complete.
            Err(AuthError::Redirect { .. }) if self.is_api_call(ctx.request.path()) => {
                Err(AuthError::unauthenticated(
                    "no credential presented; send an API token or a session cookie",
                ))
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_api_call;

    #[test]
    fn versioned_api_paths_are_api_calls() {
        assert!(is_api_call("/invokr", "/invokr/v1/jobs"));
        assert!(is_api_call("", "/v1/orgs"));
    }

    #[test]
    fn dashboard_paths_are_not() {
        // These must keep the login redirect: a person typing the URL should
        // land on the identity provider, not read a 401.
        assert!(!is_api_call("/invokr", "/dashboard/jobs"));
        assert!(!is_api_call("", "/"));
        assert!(!is_api_call("", "/dashboard/"));
    }

    #[test]
    fn a_lookalike_prefix_is_not_an_api_call() {
        // `/invokr-staging/v1/...` must not be mistaken for this deployment's
        // API just because the prefix is a string prefix of it.
        assert!(!is_api_call("/invokr", "/invokr-staging/v1/jobs"));
    }
}
