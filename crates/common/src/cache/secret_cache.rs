use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Decrypted secret values, cached per workspace.
///
/// The schema is part of the key for the same reason as [`super::ConfigCache`],
/// and with more at stake: secrets are per-workspace by design, so a cache keyed
/// on the name alone would hand one tenant's decrypted credential to another
/// tenant's dispatch for the length of the TTL.
pub struct SecretCache {
    cache: Arc<DashMap<String, (String, Instant)>>,
    ttl: Duration,
}

/// `\u{1}` cannot appear in a schema name (they are built from lowercase
/// alphanumerics and underscores), so no name can straddle the separator.
fn scoped_key(schema: &str, name: &str) -> String {
    format!("{schema}\u{1}{name}")
}

impl SecretCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            cache: Arc::new(DashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    pub fn get(&self, schema: &str, name: &str) -> Option<String> {
        let key = scoped_key(schema, name);
        if let Some(entry) = self.cache.get(&key) {
            if entry.1.elapsed() < self.ttl {
                return Some(entry.0.clone());
            }
            drop(entry);
            self.cache.remove(&key);
        }
        None
    }

    pub fn set(&self, schema: &str, name: &str, value: String) {
        self.cache
            .insert(scoped_key(schema, name), (value, Instant::now()));
    }

    pub fn invalidate(&self, schema: &str, name: &str) {
        self.cache.remove(&scoped_key(schema, name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_tenants_secret_is_never_served_to_another() {
        let cache = SecretCache::new(300);
        cache.set("org_payments", "email_api_key", "sk-payments".into());

        assert_eq!(
            cache.get("org_payments", "email_api_key").unwrap(),
            "sk-payments"
        );
        assert!(
            cache.get("org_risk", "email_api_key").is_none(),
            "a warm entry must not satisfy another workspace's lookup"
        );
    }

    #[test]
    fn same_name_in_two_schemas_keeps_two_values() {
        let cache = SecretCache::new(300);
        cache.set("org_payments", "email_api_key", "sk-payments".into());
        cache.set("org_risk", "email_api_key", "sk-risk".into());

        assert_eq!(
            cache.get("org_payments", "email_api_key").unwrap(),
            "sk-payments"
        );
        assert_eq!(cache.get("org_risk", "email_api_key").unwrap(), "sk-risk");
    }

    #[test]
    fn invalidate_only_touches_its_own_schema() {
        let cache = SecretCache::new(300);
        cache.set("org_payments", "k", "a".into());
        cache.set("org_risk", "k", "b".into());

        cache.invalidate("org_payments", "k");

        assert!(cache.get("org_payments", "k").is_none());
        assert_eq!(cache.get("org_risk", "k").unwrap(), "b");
    }

    #[test]
    fn entries_expire() {
        let cache = SecretCache::new(0);
        cache.set("org_payments", "k", "a".into());
        assert!(cache.get("org_payments", "k").is_none());
    }
}
