//! Assembling Invokr's authentication stack.
//!
//! Every failure here is returned, never panicked, so a misconfiguration
//! surfaces at startup as a message naming the variable rather than as a `401`
//! on the first request that happens to exercise it.

use std::sync::Arc;

use authn_kit::{
    adapters::actix::ActixAuthn,
    claims::{ClaimSource, IdentityClaims},
    mechanisms::{
        ApiTokenAuthenticator, BearerAuthenticator, DisabledAuthenticator, SessionAuthenticator,
    },
    oidc::{CookieSettings, LoginFlow, OidcConfig, OidcProvider},
    AuthnBuilder,
};
use invokr_common::config::{AuthEnv, AuthMode};

use crate::auth::{
    legacy::LegacyApiKeyAuthenticator,
    principal::InvokrProfile,
    routes::AuthRoutesState,
    scope::{InvokrScopes, SESSION_COOKIE},
    session::BrowserSessionAuthenticator,
};

/// Carries the CSRF state and PKCE verifier between `authorize` and the
/// callback. `HttpOnly`, so a script cannot read the verifier.
const PROTECTION_COOKIE: &str = "invokr_protection";

/// The assembled stack: the middleware, plus the state its routes need.
pub struct InvokrAuthn {
    pub middleware: ActixAuthn<InvokrProfile, InvokrScopes>,
    pub routes: AuthRoutesState,
}

/// Builds the authentication stack from configuration.
///
/// `path_prefix` is the API's mount point (`INVOKR_PATH_PREFIX`), used to derive
/// the OIDC callback URL.
pub async fn build(
    auth: &AuthEnv,
    path_prefix: &str,
    dashboard_prefix: &str,
) -> Result<InvokrAuthn, String> {
    let scopes = InvokrScopes::new(path_prefix, dashboard_prefix);
    let builder = AuthnBuilder::<InvokrProfile, _>::new(scopes);

    let (gateway, routes) = match auth.mode {
        AuthMode::Disabled => {
            let gateway = builder
                .with(DisabledAuthenticator::new(development_identity()))
                .build()
                .map_err(|e| e.to_string())?;
            (gateway, AuthRoutesState::disabled(path_prefix))
        }

        AuthMode::Oidc => {
            let oidc = auth
                .oidc
                .as_ref()
                .ok_or("INVOKR_AUTH_MODE=oidc but no OIDC configuration was read")?;

            let redirect_url = format!(
                "{}{}/oidc/login",
                oidc.redirect_host.trim_end_matches('/'),
                path_prefix
            );

            let mut config = OidcConfig::new(
                oidc.issuer_url.clone(),
                oidc.client_id.clone(),
                redirect_url,
            )
            .map_err(|e| e.to_string())?;
            if let Some(secret) = &oidc.client_secret {
                config = config.with_client_secret(secret.clone());
            }

            let provider =
                Arc::new(OidcProvider::discover(config).await.map_err(|e| {
                    format!("OIDC discovery against {} failed: {e}", oidc.issuer_url)
                })?);

            // The API and the dashboard mount under *sibling* prefixes
            // (`INVOKR_PATH_PREFIX` and `INVOKR_DASHBOARD_PATH_PREFIX`), not
            // nested ones. A cookie scoped to either would not be sent to the
            // other, so the user would sign in successfully and land on a
            // dashboard that is immediately signed out. RFC 6265 also requires a
            // matching `Path` to *overwrite* a cookie, so the same mistake
            // breaks logout. Hence `/`.
            let session = CookieSettings::session(SESSION_COOKIE)
                .with_path("/")
                .with_secure(auth.secure_cookies);
            let protection = CookieSettings::protection(PROTECTION_COOKIE)
                .with_path("/")
                .with_secure(auth.secure_cookies);

            let login = Arc::new(
                LoginFlow::new(provider.clone())
                    .with_session_cookie(session)
                    .with_protection_cookie(protection),
            );

            let mut builder = builder;

            // Transitional: accepted only while `INVOKR_API_KEY` is still set.
            if let Some(key) = &auth.legacy_api_key {
                tracing::warn!(
                    "INVOKR_API_KEY is set, so the pre-OIDC shared key is still accepted. \
                     Remove the variable once every caller holds its own credential."
                );
                builder = builder.with(LegacyApiKeyAuthenticator::new(key.clone()));
            }

            if let (Some(prefix), Some(tokens)) = (&auth.api_token_prefix, &auth.static_tokens) {
                let api_tokens =
                    ApiTokenAuthenticator::<InvokrProfile>::from_json(prefix.clone(), tokens)
                        .map_err(|e| format!("INVOKR_API_STATIC_TOKENS is invalid: {e}"))?;
                tracing::info!(
                    "static API tokens enabled: {} token(s) under prefix {:?}",
                    api_tokens.token_count(),
                    api_tokens.prefix()
                );
                builder = builder.with(api_tokens);
            }

            let gateway = builder
                .with(BearerAuthenticator::new(provider.clone()))
                // Last: the only mechanism that turns an *absent* credential
                // into a response rather than declining. Wrapped so the login
                // redirect applies to the dashboard but not to `/v1/*`, where
                // the caller is a program that cannot complete one.
                .with(BrowserSessionAuthenticator::new(
                    SessionAuthenticator::new(provider.clone()).with_login_redirect(login.clone()),
                    path_prefix,
                ))
                .build()
                .map_err(|e| e.to_string())?;

            (gateway, AuthRoutesState::enabled(path_prefix, login))
        }
    };

    tracing::info!(
        "authentication mode {:?}, mechanisms: {:?}",
        auth.mode,
        gateway.authenticator_names()
    );

    // No `with_extensions` hook: the actix adapter already inserts `P::User`
    // into the request extensions, which is exactly what `AuthenticatedRequest`
    // reads. A hook here would insert a second, identical copy.
    let middleware = ActixAuthn::new(gateway);

    Ok(InvokrAuthn { middleware, routes })
}

/// The identity every request resolves to under `INVOKR_AUTH_MODE=disabled`.
///
/// Conspicuously named on purpose: if this ever reaches a real environment it
/// should stand out in an audit log rather than blend in with real users.
fn development_identity() -> IdentityClaims {
    IdentityClaims::new(ClaimSource::StaticToken)
        .with_subject("invokr-auth-disabled")
        .with_preferred_username("invokr-auth-disabled")
        .with_email("invokr-auth-disabled@invalid")
}
