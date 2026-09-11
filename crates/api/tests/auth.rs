//! Tests for the authentication wiring.
//!
//! These cover the decisions that are ours rather than `authn_kit`'s: which
//! routes are public, how claims from each mechanism become a caller, and the
//! two places where a silent mismatch would break authentication without
//! producing an error anywhere.

use authn_kit::{
    claims::{ClaimSource, IdentityClaims},
    AuthRequest, AuthScope, ScopeResolver,
};

// The binary's modules, compiled into the test target. Most of the module is
// unreferenced from here — only the scope resolver and the principal
// conversion are exercised — so the usual dead-code lint is not meaningful.
#[allow(dead_code, unused_imports)]
#[path = "../src/auth/mod.rs"]
mod auth;

use auth::{
    principal::Caller,
    scope::{InvokrScope, InvokrScopes, SESSION_COOKIE},
};

fn request(path: &str) -> AuthRequest {
    AuthRequest::builder().method("GET").path(path).build()
}

// ---------------------------------------------------------------- scoping

#[test]
fn health_and_metrics_are_public_under_a_path_prefix() {
    let scopes = InvokrScopes::new("/invokr", "/dashboard");

    assert!(scopes.resolve(&request("/invokr/health")).is_public());
    // `/metrics` is mounted *inside* the API prefix. Testing a bare "/metrics"
    // would pass against an unprefixed deployment and leave a prefixed one
    // serving 401s to Prometheus, which reports as missing data rather than an
    // error.
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
    // If the callback required authentication, the session authenticator would
    // redirect it into the login flow it is trying to complete — an infinite
    // loop that only appears once a real provider is configured.
    let scopes = InvokrScopes::new("/invokr", "/dashboard");
    assert!(scopes.resolve(&request("/invokr/oidc/login")).is_public());
}

#[test]
fn the_callback_path_matches_the_registered_redirect_uri() {
    // `setup::build` derives the redirect URI it registers with the provider
    // from the same prefix. If these two ever diverge, the provider redirects
    // to a path that requires authentication.
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
    // `LoginFlow` writes the one named in `CookieSettings`. If the name varied
    // per scope, a session established on one route would be invisible on
    // another — login would appear to succeed and every request stay anonymous.
    assert_eq!(InvokrScope::Public.session_cookie_name(), SESSION_COOKIE);
    assert_eq!(InvokrScope::Protected.session_cookie_name(), SESSION_COOKIE);
}

// ------------------------------------------------------------- principals

#[test]
fn an_id_token_authenticates_as_its_email() {
    let claims = IdentityClaims::new(ClaimSource::IdToken)
        .with_subject("110248495921238986420")
        .with_email("someone@juspay.in");

    let caller = Caller::try_from(claims).expect("id token yields a caller");
    assert_eq!(caller.principal, "someone@juspay.in");
    assert!(!caller.legacy);
}

#[test]
fn preferred_username_wins_over_email() {
    let claims = IdentityClaims::new(ClaimSource::IdToken)
        .with_preferred_username("someone")
        .with_email("someone@juspay.in");

    assert_eq!(Caller::try_from(claims).unwrap().principal, "someone");
}

#[test]
fn an_issuer_supplying_only_a_subject_still_authenticates() {
    // Requiring `email` would quietly make Invokr single-provider: not every
    // issuer supplies one, and `preferred_username` is a Keycloak convention.
    let claims = IdentityClaims::new(ClaimSource::IdToken).with_subject("opaque-subject-id");

    assert_eq!(
        Caller::try_from(claims).unwrap().principal,
        "opaque-subject-id"
    );
}

#[test]
fn a_machine_grant_is_named_after_its_client() {
    // `client_credentials` carries no human identity at all, so demanding an
    // email would reject every machine caller.
    let claims =
        IdentityClaims::new(ClaimSource::ClientCredentials).with_client_id("reporting-svc");

    let caller = Caller::try_from(claims).unwrap();
    assert_eq!(caller.principal, "service-account-reporting-svc");
    assert_eq!(caller.source, ClaimSource::ClientCredentials);
}

#[test]
fn a_machine_grant_without_a_client_id_is_rejected() {
    let claims = IdentityClaims::new(ClaimSource::ClientCredentials);
    assert!(Caller::try_from(claims).is_err());
}

#[test]
fn a_static_token_authenticates_as_its_configured_principal() {
    let claims = IdentityClaims::new(ClaimSource::StaticToken).with_preferred_username("aarokya");

    let caller = Caller::try_from(claims).unwrap();
    assert_eq!(caller.principal, "aarokya");
    assert!(!caller.legacy);
}

#[test]
fn claims_carrying_no_identity_at_all_are_rejected() {
    let claims = IdentityClaims::new(ClaimSource::IdToken);
    assert!(Caller::try_from(claims).is_err());
}

#[test]
fn the_legacy_key_is_identifiable_in_logs() {
    // While the shared key is being retired, every request still using it must
    // be greppable, so the cutover can be verified rather than assumed.
    let caller = Caller::legacy_api_key();
    assert!(caller.legacy);
    assert_eq!(caller.principal, "legacy-api-key");
}
