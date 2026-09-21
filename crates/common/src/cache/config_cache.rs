use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Config values, cached per workspace.
///
/// The schema is part of the key, not an assumption. One worker sweeps every
/// active workspace with one cache, and config names are chosen per workspace —
/// two tenants both calling a config `email-service` is the normal case, not an
/// edge case. Keyed on the name alone, the first tenant to warm an entry would
/// serve its values to every other tenant until the TTL expired.
pub struct ConfigCache {
    cache: Arc<DashMap<String, (serde_json::Value, Instant)>>,
    ttl: Duration,
}

/// `\u{1}` cannot appear in a schema name (they are built from lowercase
/// alphanumerics and underscores), so no name can straddle the separator.
fn scoped_key(schema: &str, name: &str) -> String {
    format!("{schema}\u{1}{name}")
}

impl ConfigCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            cache: Arc::new(DashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    pub fn get(&self, schema: &str, name: &str) -> Option<serde_json::Value> {
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

    pub fn set(&self, schema: &str, name: &str, values: serde_json::Value) {
        self.cache
            .insert(scoped_key(schema, name), (values, Instant::now()));
    }

    pub fn invalidate(&self, schema: &str, name: &str) {
        self.cache.remove(&scoped_key(schema, name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_name_in_two_schemas_does_not_collide() {
        let cache = ConfigCache::new(60);
        cache.set(
            "org_payments",
            "email-service",
            serde_json::json!({ "sender": "payments" }),
        );
        cache.set(
            "org_risk",
            "email-service",
            serde_json::json!({ "sender": "risk" }),
        );

        assert_eq!(
            cache.get("org_payments", "email-service").unwrap()["sender"],
            "payments"
        );
        assert_eq!(
            cache.get("org_risk", "email-service").unwrap()["sender"],
            "risk"
        );
    }

    #[test]
    fn a_warm_entry_is_not_served_to_another_schema() {
        let cache = ConfigCache::new(60);
        cache.set(
            "org_payments",
            "email-service",
            serde_json::json!({ "sender": "payments" }),
        );
        assert!(cache.get("org_risk", "email-service").is_none());
    }

    #[test]
    fn invalidate_only_touches_its_own_schema() {
        let cache = ConfigCache::new(60);
        cache.set("org_payments", "cfg", serde_json::json!({ "v": 1 }));
        cache.set("org_risk", "cfg", serde_json::json!({ "v": 2 }));

        cache.invalidate("org_payments", "cfg");

        assert!(cache.get("org_payments", "cfg").is_none());
        assert_eq!(cache.get("org_risk", "cfg").unwrap()["v"], 2);
    }

    #[test]
    fn entries_expire() {
        let cache = ConfigCache::new(0);
        cache.set("org_payments", "cfg", serde_json::json!({ "v": 1 }));
        assert!(cache.get("org_payments", "cfg").is_none());
    }
}
