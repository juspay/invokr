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
