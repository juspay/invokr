use crate::cache::SecretCache;
use crate::db::{self, DbContext};
use std::collections::HashMap;

/// Extract all `{{secret.<name>}}` references from a JSON spec string.
/// Returns deduplicated names in order of first appearance.
pub fn extract_referenced_secret_names(spec: &serde_json::Value) -> Vec<String> {
    let spec_str = spec.to_string();
    let mut names = Vec::new();
    let mut start = 0;
    while let Some(pos) = spec_str[start..].find("{{secret.") {
        let abs = start + pos + 9; // skip past "{{secret."
        if let Some(end) = spec_str[abs..].find("}}") {
            let name = spec_str[abs..abs + end].to_string();
            if !names.contains(&name) {
                names.push(name);
            }
            start = abs + end + 2;
        } else {
            break;
        }
    }
    names
}

/// Load and decrypt all secrets referenced in `spec`.
///
/// When `cache` is `Some`, resolved values are served from / stored into the
/// cache so that repeated calls within the same request pay only one DB round-
/// trip per unique secret name.  Pass `None` for uncached callers (e.g. the
/// API server on the cancel / status path).
pub async fn load(
    db: &mut DbContext<'_>,
    encryption_key: &str,
    spec: &serde_json::Value,
    cache: Option<&SecretCache>,
) -> Result<HashMap<String, String>, String> {
    let names = extract_referenced_secret_names(spec);
    let mut out = HashMap::with_capacity(names.len());
    for name in names {
        if let Some(c) = cache {
            if let Some(v) = c.get(&name) {
                out.insert(name, v);
                continue;
            }
        }
        let secret = db::secrets::get(db, &name)
            .await
            .map_err(|e| format!("Failed to load secret '{name}': {e}"))?
            .ok_or_else(|| format!("Secret '{name}' not found"))?;
        let plain = crate::crypto::decrypt(&secret.encrypted_value, encryption_key)
            .map_err(|e| format!("Failed to decrypt secret '{name}': {e}"))?;
        if let Some(c) = cache {
            c.set(name.clone(), plain.clone());
        }
        out.insert(name, plain);
    }
    Ok(out)
}
