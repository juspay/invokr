//! Pluggable decryption for sensitive environment variables.
//!
//! A provider receives ciphertext and returns plaintext. It never reads the
//! environment -- `config::SensitiveEnvReader` owns that, so a provider is a
//! pure function of its input and can be tested without touching process
//! state. `NoEncryption` is compiled in unconditionally and is the default, so
//! every deployment runs the same code path whether or not it encrypts.

mod aws;
mod gcp;
mod no_encryption;

pub use aws::AwsKms;
pub use gcp::GcpKms;
pub use no_encryption::NoEncryption;

use crate::env::get_from_env_or_default;

// `async_trait` rewrites each `async fn` into a `#[must_use]` boxed future, and
// the methods already return a `#[must_use]` Result, which clippy 1.99 reports
// as `double_must_use`. The attribute is macro-generated, so there is nothing to
// remove; native `async fn` in traits is not an option because the trait must
// stay dyn-compatible for `Box<dyn SecretProvider>`.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait SecretProvider: Send + Sync {
    /// Decrypt one value. `name` is used only to name the variable in errors.
    async fn decrypt(&self, name: &str, ciphertext: &str) -> Result<String, SecretsError>;

    /// Confirm the provider's configuration and credentials are usable. Called
    /// once at startup, before any secret is read, so that "credentials are
    /// wrong" stays distinguishable from "this ciphertext is wrong".
    async fn validate(&self) -> Result<(), SecretsError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("{name} is not valid base64")]
    NotBase64 { name: String },
    #[error("{name} could not be decrypted: {source_msg}")]
    DecryptFailed { name: String, source_msg: String },
    #[error("{name} decrypted to invalid UTF-8")]
    NotUtf8 { name: String },
    #[error("secrets provider configuration is unusable: {0}")]
    InvalidConfig(String),
    #[error(
        "INVOKR_SECRETS_MANAGER '{0}' is not recognised; expected one of: \
         no_encryption, aws_kms, gcp_kms"
    )]
    UnknownProvider(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProviderKind {
    NoEncryption,
    AwsKms,
    GcpKms,
}

/// Parse `INVOKR_SECRETS_MANAGER`. Case-insensitive; empty means the default.
pub fn parse_provider_kind(raw: &str) -> Result<ProviderKind, SecretsError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "no_encryption" => Ok(ProviderKind::NoEncryption),
        "aws_kms" => Ok(ProviderKind::AwsKms),
        "gcp_kms" => Ok(ProviderKind::GcpKms),
        other => Err(SecretsError::UnknownProvider(other.to_owned())),
    }
}

/// Build the provider named by `INVOKR_SECRETS_MANAGER` and validate it.
pub async fn provider_from_env() -> Result<Box<dyn SecretProvider>, SecretsError> {
    let raw: String = get_from_env_or_default("INVOKR_SECRETS_MANAGER", String::new());
    let provider: Box<dyn SecretProvider> = match parse_provider_kind(&raw)? {
        ProviderKind::NoEncryption => Box::new(NoEncryption),
        ProviderKind::AwsKms => {
            tracing::info!("secrets manager: aws_kms");
            Box::new(AwsKms::new().await)
        }
        ProviderKind::GcpKms => {
            tracing::info!("secrets manager: gcp_kms");
            Box::new(GcpKms::new().await?)
        }
    };
    provider.validate().await?;
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_no_encryption_when_unset() {
        assert_eq!(parse_provider_kind("").unwrap(), ProviderKind::NoEncryption);
    }

    #[test]
    fn parses_no_encryption_case_insensitively() {
        assert_eq!(
            parse_provider_kind("NO_ENCRYPTION").unwrap(),
            ProviderKind::NoEncryption
        );
        assert_eq!(
            parse_provider_kind("no_encryption").unwrap(),
            ProviderKind::NoEncryption
        );
    }

    #[test]
    fn parses_aws_kms_case_insensitively() {
        assert_eq!(
            parse_provider_kind("AWS_KMS").unwrap(),
            ProviderKind::AwsKms
        );
        assert_eq!(
            parse_provider_kind("aws_kms").unwrap(),
            ProviderKind::AwsKms
        );
    }

    #[test]
    fn parses_gcp_kms_case_insensitively() {
        assert_eq!(
            parse_provider_kind("GCP_KMS").unwrap(),
            ProviderKind::GcpKms
        );
        assert_eq!(
            parse_provider_kind("gcp_kms").unwrap(),
            ProviderKind::GcpKms
        );
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        assert_eq!(
            parse_provider_kind("  aws_kms  ").unwrap(),
            ProviderKind::AwsKms
        );
    }

    #[test]
    fn rejects_an_unrecognised_value_and_lists_the_valid_ones() {
        let err = parse_provider_kind("bogus").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bogus"),
            "message should name the bad value: {msg}"
        );
        assert!(
            msg.contains("no_encryption") && msg.contains("aws_kms") && msg.contains("gcp_kms"),
            "message should list valid values: {msg}"
        );
    }

    #[test]
    fn rejects_the_removed_kms_enabled_spelling() {
        // Guards against anyone carrying the old boolean over verbatim.
        assert!(parse_provider_kind("true").is_err());
    }

    /// Covers the real entry point on its default path: no variable set means
    /// a provider that builds, passes `validate()` and returns plaintext
    /// unchanged.
    ///
    /// Only the default path is covered, and only when the variable is
    /// genuinely absent. `std::env::set_var` would race every other test
    /// thread calling `std::env::var`, and `serial_test` is not a dependency,
    /// so this test never mutates the environment. When the variable is set it
    /// says so rather than asserting something it cannot control.
    #[tokio::test]
    async fn default_provider_round_trips_plaintext() {
        if std::env::var_os("INVOKR_SECRETS_MANAGER").is_some() {
            eprintln!(
                "skipped: INVOKR_SECRETS_MANAGER is set in this environment; \
                 unsetting it would race other threads reading the environment"
            );
            return;
        }

        let provider = provider_from_env()
            .await
            .expect("the default provider must build and validate");

        assert_eq!(
            provider
                .decrypt("INVOKR_DATABASE_URL", "postgresql://u:p@h/db")
                .await
                .unwrap(),
            "postgresql://u:p@h/db"
        );
    }
}
