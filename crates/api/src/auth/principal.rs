//! What an authenticated caller is, and how claims from any mechanism become
//! one.
//!
//! Every mechanism `authn_kit` ships — an OIDC ID token, a JWT access token, a
//! static API token, a machine grant — normalises what it learns into
//! [`IdentityClaims`]. This module holds the single conversion that turns those
//! into Invokr's own principal, so adding a mechanism never adds a second place
//! where "who is this?" is decided.

use authn_kit::{
    claims::{ClaimSource, IdentityClaims},
    AuthProfile,
};

use crate::auth::scope::InvokrScope;

/// An authenticated caller.
///
/// Carries no authority: Invokr has no authorization layer, so every
/// authenticated caller reaches every org and workspace. This type exists to
/// name *who* acted, for audit logs and for the day an authorization layer
/// needs something to key off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// A stable, human-readable identity: an email for interactive logins, the
    /// configured principal for a static token.
    pub principal: String,
    /// Which mechanism authenticated this request.
    pub source: ClaimSource,
    /// True for the pre-OIDC shared key, so its use is greppable in logs while
    /// it is being retired.
    pub legacy: bool,
}

impl Caller {
    /// The caller behind the pre-OIDC shared `INVOKR_API_KEY`.
    ///
    /// Deliberately conspicuous: while the key is being decommissioned, every
    /// request still using it should be identifiable in logs so the cutover can
    /// be verified rather than assumed.
    pub fn legacy_api_key() -> Self {
        Self {
            principal: "legacy-api-key".to_string(),
            source: ClaimSource::StaticToken,
            legacy: true,
        }
    }
}

/// Maps claims from any mechanism onto a caller.
///
/// Deliberately permissive about *which* claim carries the identity. Requiring
/// `email` would quietly make Invokr single-provider: some issuers supply only
/// `sub`, and `preferred_username` is a Keycloak convention. `best_effort_username`
/// tries `preferred_username`, `email`, `sub`, then `client_id`.
impl TryFrom<IdentityClaims> for Caller {
    type Error = String;

    fn try_from(claims: IdentityClaims) -> Result<Self, Self::Error> {
        // A machine grant carries no human identity at all — only a validated
        // `client_id` — so it is named after the client rather than rejected
        // for having no email.
        if claims.source == ClaimSource::ClientCredentials {
            let client_id = claims
                .client_id
                .clone()
                .ok_or_else(|| String::from("client_credentials grant carried no client_id"))?;
            return Ok(Self {
                principal: format!("service-account-{client_id}"),
                source: claims.source,
                legacy: false,
            });
        }

        let principal = claims
            .best_effort_username()
            .ok_or_else(|| {
                String::from(
                    "no usable identity claim (looked for preferred_username, email, sub, client_id)",
                )
            })?
            .to_string();

        Ok(Self {
            principal,
            source: claims.source,
            legacy: false,
        })
    }
}

/// Binds the caller and scope types together for `authn_kit`.
pub struct InvokrProfile;

impl AuthProfile for InvokrProfile {
    type User = Caller;
    /// Only distinguishes "needs a credential" from "does not" — Invokr has no
    /// authorization layer, so there is no tenancy realm to model. When one is
    /// added, this scope grows the org/workspace it keys off, and that value
    /// then drives both the credential cache and the policy lookup.
    type Scope = InvokrScope;
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Requiring `email` would quietly make Invokr single-provider: not
        // every issuer supplies one, and `preferred_username` is a Keycloak
        // convention.
        let claims = IdentityClaims::new(ClaimSource::IdToken).with_subject("opaque-subject-id");

        assert_eq!(
            Caller::try_from(claims).unwrap().principal,
            "opaque-subject-id"
        );
    }

    #[test]
    fn a_machine_grant_is_named_after_its_client() {
        // `client_credentials` carries no human identity at all, so demanding
        // an email would reject every machine caller.
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
        let claims =
            IdentityClaims::new(ClaimSource::StaticToken).with_preferred_username("aarokya");

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
        // While the shared key is being retired, every request still using it
        // must be greppable, so the cutover can be verified rather than
        // assumed.
        let caller = Caller::legacy_api_key();
        assert!(caller.legacy);
        assert_eq!(caller.principal, "legacy-api-key");
    }
}
