use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use rand::RngCore;

pub fn encrypt(plaintext: &str, key_hex: &str) -> anyhow::Result<Vec<u8>> {
    let key_bytes = hex_decode(key_hex)?;
    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid key length: {}", e))?;

    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;

    // Prepend nonce to ciphertext
    let mut result = nonce_bytes.to_vec();
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

pub fn decrypt(encrypted: &[u8], key_hex: &str) -> anyhow::Result<String> {
    if encrypted.len() < 12 {
        anyhow::bail!("Encrypted data too short");
    }

    let key_bytes = hex_decode(key_hex)?;
    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid key length: {}", e))?;

    let (nonce_bytes, ciphertext) = encrypted.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("Decryption failed: {}", e))?;

    Ok(String::from_utf8(plaintext)?)
}

/// AES-256 takes a 32-byte key, written as 64 hex characters.
const KEY_HEX_LEN: usize = 64;

/// Checks a hex-encoded AES-256 key once, so a malformed key stops the
/// process at startup instead of failing the first secret read or write.
/// Error messages never include the key.
pub fn validate_key(key_hex: &str) -> anyhow::Result<()> {
    hex_decode(key_hex).map(|_| ())
}

fn hex_decode(hex: &str) -> anyhow::Result<Vec<u8>> {
    if hex.len() != KEY_HEX_LEN {
        anyhow::bail!(
            "the key must be {KEY_HEX_LEN} hex characters (32 bytes), got {}",
            hex.len()
        );
    }
    // Checked before slicing: a multibyte character would put a slice
    // boundary inside it, and `&hex[i..i + 2]` would panic.
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("the key must contain only hex characters (0-9, a-f)");
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(Into::into))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// 64 bytes, so it passes a byte-length check, but the leading ASCII byte
    /// puts every two-byte slice boundary in the middle of an `é`.
    fn misaligned_multibyte_key() -> String {
        format!("a{}b", "é".repeat(31))
    }

    #[test]
    fn test_encrypt_decrypt() {
        let plaintext = "my-secret-value";
        let encrypted = encrypt(plaintext, VALID_KEY).unwrap();
        let decrypted = decrypt(&encrypted, VALID_KEY).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn validate_key_accepts_a_random_key() {
        assert!(validate_key(VALID_KEY).is_ok());
    }

    #[test]
    fn validate_key_accepts_uppercase_hex() {
        assert!(validate_key(&VALID_KEY.to_uppercase()).is_ok());
    }

    #[test]
    fn validate_key_accepts_the_all_zero_key() {
        // Existing deployments set it explicitly; their secrets are stored under it.
        assert!(validate_key(&"0".repeat(64)).is_ok());
    }

    #[test]
    fn validate_key_rejects_a_short_key() {
        let err = validate_key(&VALID_KEY[..62]).unwrap_err().to_string();
        assert!(err.contains("64 hex characters"), "unexpected error: {err}");
    }

    #[test]
    fn validate_key_rejects_a_trailing_newline() {
        // `echo` piped into a KMS encrypt command leaves this in the plaintext.
        assert!(validate_key(&format!("{VALID_KEY}\n")).is_err());
    }

    #[test]
    fn validate_key_rejects_non_hex_characters() {
        assert!(validate_key(&"g".repeat(64)).is_err());
    }

    #[test]
    fn validate_key_rejects_multibyte_input_without_panicking() {
        assert!(validate_key(&misaligned_multibyte_key()).is_err());
    }

    #[test]
    fn encrypt_rejects_multibyte_key_without_panicking() {
        assert!(encrypt("x", &misaligned_multibyte_key()).is_err());
    }

    #[test]
    fn validate_key_errors_never_contain_the_key() {
        let almost = format!("{}zz", &VALID_KEY[..62]);
        let err = validate_key(&almost).unwrap_err().to_string();
        assert!(!err.contains(&almost[..62]), "error leaks the key: {err}");
    }
}
