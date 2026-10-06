use base64::{engine::general_purpose, Engine};
use google_cloud_kms::{
    client::{Client, ClientConfig},
    grpc::kms::v1::DecryptRequest,
};

use super::{SecretProvider, SecretsError};

/// Google Cloud KMS provider.
///
/// Credentials come from Application Default Credentials, so on GKE this is
/// Workload Identity with no explicit configuration. Unlike AWS, the key is
/// named per-request, so `INVOKR_GCP_KMS_KEY_NAME` is required.
pub struct GcpKms {
    /// `None` only in unit tests, which exercise the paths before any RPC.
    client: Option<Client>,
    key_name: String,
}

impl GcpKms {
    pub async fn new() -> Result<Self, SecretsError> {
        let key_name = std::env::var("INVOKR_GCP_KMS_KEY_NAME").map_err(|_| {
            SecretsError::InvalidConfig(
                "INVOKR_GCP_KMS_KEY_NAME must be set when \
                 INVOKR_SECRETS_MANAGER=gcp_kms; expected \
                 projects/<p>/locations/<l>/keyRings/<r>/cryptoKeys/<k>"
                    .to_owned(),
            )
        })?;

        let config = ClientConfig::default().with_auth().await.map_err(|e| {
            SecretsError::InvalidConfig(format!(
                "GCP Application Default Credentials could not be initialised: {e:?}"
            ))
        })?;

        let client = Client::new(config).await.map_err(|e| {
            SecretsError::InvalidConfig(format!("GCP KMS client could not be created: {e:?}"))
        })?;

        Ok(Self {
            client: Some(client),
            key_name,
        })
    }
}

#[async_trait::async_trait]
impl SecretProvider for GcpKms {
    async fn decrypt(&self, name: &str, ciphertext: &str) -> Result<String, SecretsError> {
        let decoded =
            general_purpose::STANDARD
                .decode(ciphertext)
                .map_err(|_| SecretsError::NotBase64 {
                    name: name.to_owned(),
                })?;

        let client = self
            .client
            .as_ref()
            .ok_or_else(|| SecretsError::InvalidConfig("GCP client not built".into()))?;

        let request = DecryptRequest {
            name: self.key_name.clone(),
            ciphertext: decoded,
            additional_authenticated_data: Vec::new(),
            ciphertext_crc32c: None,
            additional_authenticated_data_crc32c: None,
        };

        let response =
            client
                .decrypt(request, None)
                .await
                .map_err(|e| SecretsError::DecryptFailed {
                    name: name.to_owned(),
                    source_msg: format!("{e:?}"),
                })?;

        String::from_utf8(response.plaintext).map_err(|_| SecretsError::NotUtf8 {
            name: name.to_owned(),
        })
    }

    async fn validate(&self) -> Result<(), SecretsError> {
        // Checked here rather than at construction so a malformed name is a
        // startup error naming the variable, instead of an opaque gRPC
        // NOT_FOUND on the first secret read.
        if !self.key_name.starts_with("projects/") || !self.key_name.contains("/cryptoKeys/") {
            return Err(SecretsError::InvalidConfig(format!(
                "INVOKR_GCP_KMS_KEY_NAME '{}' is not a KMS key resource name; \
                 expected projects/<p>/locations/<l>/keyRings/<r>/cryptoKeys/<k>",
                self.key_name
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(key_name: &str) -> GcpKms {
        GcpKms {
            client: None,
            key_name: key_name.to_string(),
        }
    }

    #[tokio::test]
    async fn validate_accepts_a_full_key_resource_name() {
        let p = provider("projects/p/locations/l/keyRings/r/cryptoKeys/k");
        assert!(p.validate().await.is_ok());
    }

    #[tokio::test]
    async fn validate_rejects_a_bare_key_id() {
        let err = provider("my-key").validate().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("INVOKR_GCP_KMS_KEY_NAME"),
            "error should name the variable: {msg}"
        );
        assert!(
            msg.contains("my-key"),
            "error should quote the bad value: {msg}"
        );
    }

    #[tokio::test]
    async fn validate_rejects_a_key_ring_without_a_key() {
        // A real mistake: copying the key ring path and forgetting /cryptoKeys/.
        let p = provider("projects/p/locations/l/keyRings/r");
        assert!(p.validate().await.is_err());
    }

    #[tokio::test]
    async fn rejects_non_base64_before_calling_gcp() {
        // `client: None` proves no RPC is attempted.
        let p = provider("projects/p/locations/l/keyRings/r/cryptoKeys/k");
        let err = p
            .decrypt("INVOKR_DATABASE_URL", "not!valid!base64!")
            .await
            .unwrap_err();
        assert!(matches!(err, SecretsError::NotBase64 { .. }));
        assert!(err.to_string().contains("INVOKR_DATABASE_URL"));
    }
}
