use super::{SecretProvider, SecretsError};

/// Pass-through provider: the environment already holds plaintext.
///
/// Compiled in unconditionally and the default, so a deployment that does not
/// encrypt runs the same code path as one that does, rather than a bypass.
pub struct NoEncryption;

#[async_trait::async_trait]
impl SecretProvider for NoEncryption {
    async fn decrypt(&self, _name: &str, ciphertext: &str) -> Result<String, SecretsError> {
        Ok(ciphertext.to_owned())
    }

    async fn validate(&self) -> Result<(), SecretsError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passes_the_value_through_unchanged() {
        let p = NoEncryption;
        assert_eq!(
            p.decrypt("INVOKR_DATABASE_URL", "postgresql://u:p@h/db")
                .await
                .unwrap(),
            "postgresql://u:p@h/db"
        );
    }

    #[tokio::test]
    async fn validates_without_configuration() {
        assert!(NoEncryption.validate().await.is_ok());
    }

    #[tokio::test]
    async fn passes_an_empty_value_through() {
        // The reader filters blanks; the provider must not second-guess it.
        assert_eq!(NoEncryption.decrypt("X", "").await.unwrap(), "");
    }
}
