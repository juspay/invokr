//! The pre-OIDC shared API key — a transitional mechanism, not a mode.
//!
//! Before this work, `INVOKR_API_KEY` was the only credential Invokr accepted,
//! and it was rendered into the dashboard's HTML. That leak is closed the moment
//! this ships, because the dashboard is no longer handed a key at all.
//!
//! The key itself lingers only so existing machine callers keep working while
//! they are issued their own tokens. It is registered as one more element of
//! the chain, gated on `INVOKR_API_KEY` still being set — so **removing the
//! variable decommissions it**, with no redeploy and no code change.
//!
//! Delete this file once every environment has dropped the variable.

use async_trait::async_trait;
use authn_kit::{
    authenticator::AuthProfile, AuthContext, AuthError, Authenticator, Credential, Verdict,
};
use secrecy::{ExposeSecret, SecretString};

use crate::auth::principal::{Caller, InvokrProfile};

pub struct LegacyApiKeyAuthenticator {
    key: SecretString,
}

impl LegacyApiKeyAuthenticator {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: SecretString::from(key.into()),
        }
    }
}

impl std::fmt::Debug for LegacyApiKeyAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LegacyApiKeyAuthenticator { key: <redacted> }")
    }
}

#[async_trait]
impl Authenticator<InvokrProfile> for LegacyApiKeyAuthenticator {
    fn name(&self) -> &'static str {
        "legacy-api-key"
    }

    async fn authenticate(
        &self,
        ctx: &AuthContext<'_, InvokrProfile>,
    ) -> Result<Verdict<<InvokrProfile as AuthProfile>::User>, AuthError> {
        let Credential::Bearer(token) = ctx.credential else {
            return Ok(Verdict::NotApplicable);
        };

        // A mismatch declines rather than fails. The chain is fail-closed — an
        // `Err` from any authenticator ends it immediately — so rejecting here
        // would stop a perfectly good JWT or `ivk_` token from ever being tried,
        // purely because it is not this key.
        if !keys_match(token.expose_secret(), self.key.expose_secret()) {
            return Ok(Verdict::NotApplicable);
        }

        Ok(Verdict::Authenticated(Caller::legacy_api_key()))
    }
}

/// Constant-time comparison. `==` on strings returns at the first differing
/// byte, a timing side channel on the one key that unlocks every tenant.
/// Comparing SHA-256 digests also keeps the time independent of the presented
/// token's length.
fn keys_match(presented: &str, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;

    Sha256::digest(presented.as_bytes())
        .ct_eq(&Sha256::digest(expected.as_bytes()))
        .into()
}

#[cfg(test)]
mod tests {
    use super::keys_match;

    #[test]
    fn matches_only_the_exact_key() {
        assert!(keys_match("dev-api-key", "dev-api-key"));
        assert!(!keys_match("dev-api-kez", "dev-api-key"));
        assert!(!keys_match("dev-api-key-longer", "dev-api-key"));
        assert!(!keys_match("dev", "dev-api-key"));
        assert!(!keys_match("", "dev-api-key"));
    }
}
