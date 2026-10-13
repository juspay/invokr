use base64::{engine::general_purpose, Engine};
use oci_kms::{OciKmsClient, OciKmsConfig, OciKmsError};

use super::{SecretProvider, SecretsError};

const ENDPOINT_VAR: &str = "INVOKR_OCI_KMS_CRYPTO_ENDPOINT";
const KEY_ID_VAR: &str = "INVOKR_OCI_KMS_KEY_ID";

/// OCI Vault KMS provider.
///
/// Inside Kubernetes, credentials come from OKE Workload Identity, which needs an
/// Enhanced cluster; outside it, from the `oci` CLI's `~/.oci/config`. Both are
/// resolved by the `oci_kms` crate on the first decrypt, not at construction.
/// Like GCP, the key is named per request, so both variables are required.
pub struct OciKms {
    /// `None` only in unit tests, which exercise the paths before any request.
    client: Option<OciKmsClient>,
    config: OciKmsConfig,
}

impl OciKms {
    pub fn new() -> Result<Self, SecretsError> {
        Self::from_config(config_from(|name| std::env::var(name).ok())?)
    }

    fn from_config(config: OciKmsConfig) -> Result<Self, SecretsError> {
        // Only a bad URL is the endpoint variable's fault; a TLS or HTTP client
        // build failure is not, so it isn't attributed to it.
        let client = OciKmsClient::new(&config).map_err(|e| match e {
            OciKmsError::InvalidConfig(_) => {
                SecretsError::InvalidConfig(format!("{ENDPOINT_VAR}: {e}"))
            }
            other => SecretsError::InvalidConfig(other.to_string()),
        })?;
        Ok(Self {
            client: Some(client),
            config,
        })
    }
}

/// Read both variables through `lookup`, so tests need not touch the environment.
fn config_from(lookup: impl Fn(&str) -> Option<String>) -> Result<OciKmsConfig, SecretsError> {
    let required = |name: &str, expected: &str| {
        lookup(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                SecretsError::InvalidConfig(format!(
                    "{name} must be set when INVOKR_SECRETS_MANAGER=oci_kms; expected {expected}"
                ))
            })
    };
    Ok(OciKmsConfig {
        vault_crypto_endpoint: required(
            ENDPOINT_VAR,
            "the vault's crypto endpoint, https://<vault>-crypto.kms.<region>.oraclecloud.com",
        )?,
        key_id: required(KEY_ID_VAR, "the key OCID, ocid1.key.oc1...")?,
    })
}

/// Credentials resolve lazily, so the first decrypt is where a missing service
/// account token or a denied IAM policy first surfaces. Those are configuration
/// faults, not this variable's, so they must not be reported as if its
/// ciphertext were wrong. OCI answers a missing policy with 404
/// `NotAuthorizedOrNotFound` as often as with 401 or 403.
fn decrypt_error(name: &str, error: OciKmsError) -> SecretsError {
    match error {
        OciKmsError::CredentialsUnavailable(_)
        | OciKmsError::SigningFailed(_)
        | OciKmsError::UnexpectedStatus {
            status: 401 | 403 | 404,
            ..
        } => SecretsError::InvalidConfig(format!(
            "OCI KMS credentials or IAM policy are not usable: {error}"
        )),
        other => SecretsError::DecryptFailed {
            name: name.to_owned(),
            source_msg: other.to_string(),
        },
    }
}

#[async_trait::async_trait]
impl SecretProvider for OciKms {
    async fn decrypt(&self, name: &str, ciphertext: &str) -> Result<String, SecretsError> {
        // OCI takes the base64 text as-is; decoding here only rejects a bad value
        // locally, naming the variable, instead of as an HTTP 400 from the vault.
        general_purpose::STANDARD
            .decode(ciphertext)
            .map_err(|_| SecretsError::NotBase64 {
                name: name.to_owned(),
            })?;

        let client = self
            .client
            .as_ref()
            .ok_or_else(|| SecretsError::InvalidConfig("OCI client not built".into()))?;

        let plaintext = client
            .decrypt(ciphertext)
            .await
            .map_err(|e| decrypt_error(name, e))?;

        String::from_utf8(plaintext).map_err(|_| SecretsError::NotUtf8 {
            name: name.to_owned(),
        })
    }

    async fn validate(&self) -> Result<(), SecretsError> {
        // Checked here so each mistake is a startup error naming the variable,
        // instead of an opaque 4xx from the vault on the first secret read.
        let endpoint = &self.config.vault_crypto_endpoint;
        let is_https = endpoint
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"));
        if !is_https {
            return Err(SecretsError::InvalidConfig(format!(
                "{ENDPOINT_VAR} '{endpoint}' must be an https:// URL"
            )));
        }
        if endpoint.contains("-management.") {
            return Err(SecretsError::InvalidConfig(format!(
                "{ENDPOINT_VAR} '{endpoint}' is the vault's management endpoint; \
                 use its crypto endpoint, https://<vault>-crypto.kms.<region>.oraclecloud.com"
            )));
        }
        let key_id = &self.config.key_id;
        if !key_id.starts_with("ocid1.key.") {
            return Err(SecretsError::InvalidConfig(format!(
                "{KEY_ID_VAR} '{key_id}' is not a key OCID; expected ocid1.key.oc1..."
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENDPOINT: &str = "https://abc-crypto.kms.ap-mumbai-1.oraclecloud.com";
    const KEY_ID: &str = "ocid1.key.oc1.ap-mumbai-1.abc.xyz";

    /// Stands in for the environment, so these tests never mutate process state.
    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn provider(endpoint: &str, key_id: &str) -> OciKms {
        OciKms {
            client: None,
            config: OciKmsConfig {
                vault_crypto_endpoint: endpoint.to_owned(),
                key_id: key_id.to_owned(),
            },
        }
    }

    #[test]
    fn reads_the_endpoint_and_key_id() {
        let config = config_from(lookup(&[(ENDPOINT_VAR, ENDPOINT), (KEY_ID_VAR, KEY_ID)]))
            .expect("both variables are set");
        assert_eq!(config.vault_crypto_endpoint, ENDPOINT);
        assert_eq!(config.key_id, KEY_ID);
    }

    #[test]
    fn a_missing_endpoint_names_the_variable() {
        let err = config_from(lookup(&[(KEY_ID_VAR, KEY_ID)])).unwrap_err();
        assert!(matches!(err, SecretsError::InvalidConfig(_)));
        assert!(err.to_string().contains(ENDPOINT_VAR), "{err}");
    }

    #[test]
    fn a_missing_key_id_names_the_variable() {
        let err = config_from(lookup(&[(ENDPOINT_VAR, ENDPOINT)])).unwrap_err();
        assert!(err.to_string().contains(KEY_ID_VAR), "{err}");
    }

    #[test]
    fn a_blank_value_counts_as_missing() {
        let err = config_from(lookup(&[(ENDPOINT_VAR, "  "), (KEY_ID_VAR, KEY_ID)])).unwrap_err();
        assert!(err.to_string().contains(ENDPOINT_VAR), "{err}");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        // A value pasted into an env file or ConfigMap often carries a newline.
        let endpoint = format!(" {ENDPOINT}\n");
        let key_id = format!("{KEY_ID} ");
        let config = config_from(lookup(&[
            (ENDPOINT_VAR, endpoint.as_str()),
            (KEY_ID_VAR, key_id.as_str()),
        ]))
        .unwrap();
        assert_eq!(config.vault_crypto_endpoint, ENDPOINT);
        assert_eq!(config.key_id, KEY_ID);
    }

    #[test]
    fn credential_failures_are_reported_as_configuration() {
        // Credentials resolve lazily, so the first decrypt is where a missing
        // service account token surfaces. Blaming that variable would mislead.
        let err = decrypt_error(
            "INVOKR_DATABASE_URL",
            OciKmsError::CredentialsUnavailable("no service account token".to_owned()),
        );
        assert!(matches!(err, SecretsError::InvalidConfig(_)), "{err}");
        assert!(!err.to_string().contains("INVOKR_DATABASE_URL"), "{err}");
    }

    #[test]
    fn authorization_rejections_are_reported_as_configuration() {
        // OCI answers a missing IAM policy with 404 NotAuthorizedOrNotFound as
        // often as with 401 or 403.
        for status in [401, 403, 404] {
            let err = decrypt_error(
                "INVOKR_DATABASE_URL",
                OciKmsError::UnexpectedStatus {
                    status,
                    body: r#"{"code":"NotAuthorizedOrNotFound"}"#.to_owned(),
                },
            );
            assert!(
                matches!(err, SecretsError::InvalidConfig(_)),
                "HTTP {status}: {err}"
            );
        }
    }

    #[test]
    fn other_failures_name_the_variable() {
        let err = decrypt_error(
            "INVOKR_DATABASE_URL",
            OciKmsError::UnexpectedStatus {
                status: 400,
                body: r#"{"code":"InvalidParameter"}"#.to_owned(),
            },
        );
        assert!(matches!(err, SecretsError::DecryptFailed { .. }), "{err}");
        assert!(err.to_string().contains("INVOKR_DATABASE_URL"), "{err}");
    }

    #[test]
    fn a_valid_config_builds_a_client_without_network() {
        let p = OciKms::from_config(OciKmsConfig {
            vault_crypto_endpoint: ENDPOINT.to_owned(),
            key_id: KEY_ID.to_owned(),
        })
        .expect("a valid config must build");
        assert!(p.client.is_some());
    }

    #[test]
    fn an_unparseable_endpoint_names_the_variable() {
        let err = OciKms::from_config(OciKmsConfig {
            vault_crypto_endpoint: "not a url".to_owned(),
            key_id: KEY_ID.to_owned(),
        })
        .err()
        .expect("an unparseable endpoint must be rejected");
        assert!(matches!(err, SecretsError::InvalidConfig(_)));
        assert!(err.to_string().contains(ENDPOINT_VAR), "{err}");
    }

    #[tokio::test]
    async fn validate_accepts_a_crypto_endpoint_and_a_key_ocid() {
        assert!(provider(ENDPOINT, KEY_ID).validate().await.is_ok());
    }

    #[tokio::test]
    async fn validate_accepts_an_uppercase_scheme() {
        let p = provider("HTTPS://abc-crypto.kms.ap-mumbai-1.oraclecloud.com", KEY_ID);
        assert!(p.validate().await.is_ok());
    }

    #[tokio::test]
    async fn validate_accepts_a_trailing_slash() {
        let p = provider(&format!("{ENDPOINT}/"), KEY_ID);
        assert!(p.validate().await.is_ok());
    }

    #[tokio::test]
    async fn validate_accepts_another_realm() {
        // US Government Cloud: a different domain and realm, same shapes.
        let p = provider(
            "https://abc-crypto.kms.us-langley-1.oraclegovcloud.com",
            "ocid1.key.oc2.us-langley-1.abc",
        );
        assert!(p.validate().await.is_ok());
    }

    #[tokio::test]
    async fn validate_rejects_a_plain_http_endpoint() {
        let p = provider("http://abc-crypto.kms.ap-mumbai-1.oraclecloud.com", KEY_ID);
        let err = p.validate().await.unwrap_err();
        assert!(err.to_string().contains(ENDPOINT_VAR), "{err}");
        assert!(err.to_string().contains("https"), "{err}");
    }

    #[tokio::test]
    async fn validate_rejects_the_management_endpoint() {
        // A real mistake: the vault page lists both endpoints side by side, and
        // the management one answers crypto calls with an opaque 404.
        let p = provider(
            "https://abc-management.kms.ap-mumbai-1.oraclecloud.com",
            KEY_ID,
        );
        let err = p.validate().await.unwrap_err();
        assert!(err.to_string().contains("crypto endpoint"), "{err}");
    }

    #[tokio::test]
    async fn validate_rejects_a_vault_ocid_in_place_of_a_key_ocid() {
        // Another real mistake: the vault OCID sits next to the key OCID.
        let p = provider(ENDPOINT, "ocid1.vault.oc1.ap-mumbai-1.abc");
        let err = p.validate().await.unwrap_err();
        assert!(err.to_string().contains(KEY_ID_VAR), "{err}");
        assert!(
            err.to_string().contains("ocid1.vault.oc1.ap-mumbai-1.abc"),
            "error should quote the bad value: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_non_base64_before_calling_oci() {
        // `client: None` proves no request is attempted.
        let err = provider(ENDPOINT, KEY_ID)
            .decrypt("INVOKR_DATABASE_URL", "not!valid!base64!")
            .await
            .unwrap_err();
        assert!(matches!(err, SecretsError::NotBase64 { .. }));
        assert!(err.to_string().contains("INVOKR_DATABASE_URL"));
    }

    /// Round-trips a real encrypt/decrypt against an OCI vault.
    ///
    /// Ignored by default: there is no OCI emulator, so it needs a real vault and
    /// `oci` CLI credentials in `~/.oci/config`. Run with:
    ///   INVOKR_OCI_KMS_CRYPTO_ENDPOINT=https://<vault>-crypto.kms.<region>.oraclecloud.com \
    ///   INVOKR_OCI_KMS_KEY_ID=ocid1.key.oc1... \
    ///     cargo test -p invokr-common secrets::oci -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires an OCI vault and ~/.oci/config"]
    async fn round_trips_against_a_real_vault() {
        let provider = OciKms::new().expect("both INVOKR_OCI_KMS_* variables must be set");
        provider.validate().await.expect("validate");

        // Encrypt through the same client the provider built, as an operator would
        // with `oci kms crypto encrypt`.
        let plaintext = "postgresql://invokr:invokr@localhost:5434/invokr_db";
        let ciphertext = provider
            .client
            .as_ref()
            .expect("client built")
            .encrypt(plaintext.as_bytes())
            .await
            .expect("encrypt");

        assert_eq!(
            provider
                .decrypt("INVOKR_DATABASE_URL", &ciphertext)
                .await
                .unwrap(),
            plaintext
        );
    }
}
