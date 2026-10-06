use aws_sdk_kms::{error::ProvideErrorMetadata, primitives::Blob, Client};
use base64::{engine::general_purpose, Engine};

use super::{SecretProvider, SecretsError};

/// AWS KMS provider.
///
/// Unlike the code this replaces, the region is **not** overridden here. The
/// standard chain is used, so AWS_REGION, AWS_DEFAULT_REGION, IRSA and IMDS
/// discovery all work. The previous implementation defaulted to `us-east-1`
/// and passed it explicitly, which silently sent every request to the wrong
/// region for any key hosted elsewhere.
pub struct AwsKms {
    /// `None` only in unit tests, which exercise the paths before any call.
    client: Option<Client>,
    /// The region the chain resolved, captured so `validate` can report its
    /// absence instead of letting the SDK fail cryptically on first use.
    region: Option<String>,
}

impl AwsKms {
    pub async fn new() -> Self {
        let mut loader = aws_config::from_env();

        // Still honoured, for LocalStack and VPC endpoints. `just kms-init`
        // depends on this and is the only automated exercise of real decryption.
        if let Ok(endpoint) = std::env::var("AWS_ENDPOINT_URL") {
            tracing::info!("Using custom AWS endpoint: {endpoint}");
            loader = loader.endpoint_url(endpoint);
        }

        let config = loader.load().await;
        let region = config.region().map(|r| r.to_string());

        Self {
            client: Some(Client::new(&config)),
            region,
        }
    }
}

#[async_trait::async_trait]
impl SecretProvider for AwsKms {
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
            .ok_or_else(|| SecretsError::InvalidConfig("AWS client not built".into()))?;

        let output = client
            .decrypt()
            .ciphertext_blob(Blob::new(decoded))
            .send()
            .await
            .map_err(|e| SecretsError::DecryptFailed {
                name: name.to_owned(),
                // `e.to_string()` yields a bare "service error" and drops the
                // cause; `DisplayErrorContext` recovers it but dumps the whole
                // HTTP response as Debug. The metadata is the readable middle
                // ground, and it is what distinguishes the failures an operator
                // actually hits -- a wrong region, or a denied kms:Decrypt.
                source_msg: match (e.code(), e.message()) {
                    (Some(code), Some(msg)) => format!("{code}: {msg}"),
                    (Some(code), None) => code.to_owned(),
                    _ => aws_sdk_kms::error::DisplayErrorContext(&e).to_string(),
                },
            })?;

        let plaintext = output
            .plaintext()
            .ok_or_else(|| SecretsError::DecryptFailed {
                name: name.to_owned(),
                source_msg: "KMS returned no plaintext".to_owned(),
            })?;

        String::from_utf8(plaintext.as_ref().to_vec()).map_err(|_| SecretsError::NotUtf8 {
            name: name.to_owned(),
        })
    }

    async fn validate(&self) -> Result<(), SecretsError> {
        if self.region.is_none() {
            return Err(SecretsError::InvalidConfig(
                "no AWS region could be resolved: set AWS_REGION or \
                 AWS_DEFAULT_REGION, or run where IRSA/IMDS provides one"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn validate_fails_when_no_region_resolved() {
        let p = AwsKms {
            client: None,
            region: None,
        };
        let err = p.validate().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("region"),
            "error should mention the region: {msg}"
        );
    }

    #[tokio::test]
    async fn validate_passes_when_a_region_resolved() {
        let p = AwsKms {
            client: None,
            region: Some("eu-central-1".to_string()),
        };
        assert!(p.validate().await.is_ok());
    }

    #[tokio::test]
    async fn rejects_non_base64_before_calling_aws() {
        // `client: None` proves no network call is attempted: a base64 failure
        // must be detected first, or this test would panic on the unwrap.
        let p = AwsKms {
            client: None,
            region: Some("eu-central-1".to_string()),
        };
        let err = p
            .decrypt("INVOKR_DATABASE_URL", "not!valid!base64!")
            .await
            .unwrap_err();
        assert!(matches!(err, SecretsError::NotBase64 { .. }));
        assert!(err.to_string().contains("INVOKR_DATABASE_URL"));
    }

    /// Round-trips a real encrypt/decrypt against LocalStack.
    ///
    /// Ignored by default because it needs a running LocalStack. Run with:
    ///   just kms-up
    ///   AWS_ENDPOINT_URL=http://localhost:4566 AWS_REGION=us-east-1 \
    ///     AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test \
    ///     cargo test -p invokr-common secrets::aws -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires LocalStack: just kms-up"]
    async fn round_trips_against_localstack() {
        let provider = AwsKms::new().await;
        provider
            .validate()
            .await
            .expect("validate should pass with AWS_REGION set");

        // Create a key and encrypt through the same client the provider built.
        let client = provider.client.as_ref().expect("client built");
        let key_id = client
            .create_key()
            .send()
            .await
            .expect("create_key")
            .key_metadata()
            .expect("key metadata")
            .key_id()
            .to_owned();

        let plaintext = "postgresql://invokr:invokr@localhost:5434/invokr_db";
        let blob = client
            .encrypt()
            .key_id(&key_id)
            .plaintext(Blob::new(plaintext.as_bytes()))
            .send()
            .await
            .expect("encrypt")
            .ciphertext_blob()
            .expect("ciphertext")
            .as_ref()
            .to_vec();

        let b64 = general_purpose::STANDARD.encode(blob);

        assert_eq!(
            provider.decrypt("INVOKR_DATABASE_URL", &b64).await.unwrap(),
            plaintext
        );
    }
}
