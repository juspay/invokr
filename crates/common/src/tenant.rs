use sqlx::PgPool;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Identifies the workspace for a request.
#[derive(Debug, Clone)]
pub struct WorkspaceContext {
    pub org_id: String,
    pub workspace_id: String,
    pub schema_name: String,
}

/// Validates that a schema name contains only safe characters.
pub fn validate_schema_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validates that a table prefix contains only safe characters (empty is also valid).
pub fn validate_table_prefix(prefix: &str) -> bool {
    prefix.is_empty()
        || prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Maximum length of an org/workspace slug. Kept well under PostgreSQL's 63-byte
/// identifier limit so a `{org}_{workspace}` schema name stays addressable.
pub const MAX_SLUG_LEN: usize = 25;

/// Validates an organization or workspace slug.
///
/// Slugs are user-supplied identifiers that flow into URLs and tenant schema
/// names, so we restrict them to a safe, readable shape: lowercase ASCII
/// letters, digits, and interior hyphens (no leading/trailing hyphen), 1..=25
/// characters. This guarantees a slug is always a safe schema-name component
/// once hyphens are mapped to underscores by `build_schema_name`.
pub fn validate_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Builds the schema name from org_id and workspace slug.
/// Replaces hyphens with underscores since PostgreSQL schema names can't contain hyphens.
pub fn build_schema_name(org_id: &str, workspace_slug: &str) -> String {
    format!(
        "{}_{}",
        org_id.replace('-', "_"),
        workspace_slug.replace('-', "_")
    )
}

/// Trait for discovering active workspace schemas.
/// Implement this to tell Invokr's worker where to find the list of workspaces.
/// Invokr ships `SchemaRegistry` as the default implementation.
///
/// Declared with `-> impl Future + Send` (rather than `async fn`) so the
/// returned future is usable inside the spawned worker task; implementors
/// can simply write `async fn get_active_schemas(&self)`.
pub trait SchemaProvider: Send + Sync + 'static {
    fn get_active_schemas(&self) -> impl Future<Output = Result<Vec<String>, sqlx::Error>> + Send;
    /// Returns the `(org_id, workspace_id)` pair for the given schema name,
    /// or `None` if the schema is not known to this provider.
    fn get_org_ws(
        &self,
        schema_name: &str,
    ) -> impl Future<Output = Option<(String, String)>> + Send;
}

/// Cached registry of active workspace schemas.
/// Default `SchemaProvider` implementation — queries Invokr's own
/// `public.workspaces` table. Used by standalone Invokr.
#[derive(Clone)]
pub struct SchemaRegistry {
    pool: PgPool,
    cache: Arc<RwLock<CachedSchemas>>,
    ttl: Duration,
}

struct CachedSchemas {
    schemas: Vec<String>,
    /// Maps schema_name → (org_id, workspace_id). Populated together with `schemas`.
    org_ws_map: HashMap<String, (String, String)>,
    fetched_at: Instant,
}

impl SchemaRegistry {
    pub fn new(pool: PgPool, ttl_secs: u64) -> Self {
        Self {
            pool,
            cache: Arc::new(RwLock::new(CachedSchemas {
                schemas: Vec::new(),
                org_ws_map: HashMap::new(),
                fetched_at: Instant::now() - Duration::from_secs(ttl_secs + 1), // force initial fetch
            })),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    /// Returns the `(org_id, workspace_id)` for the given schema name.
    ///
    /// Returns the cached values if still fresh. Falls back to a live DB query
    /// if the cache is stale or the key is missing (e.g. during the very first
    /// poll cycle). Returns `None` if the schema is not found.
    pub async fn get_org_ws(
        &self,
        schema_name: &str,
    ) -> Result<Option<(String, String)>, sqlx::Error> {
        // Check in-memory cache first.
        {
            let cache = self.cache.read().await;
            if cache.fetched_at.elapsed() < self.ttl {
                return Ok(cache.org_ws_map.get(schema_name).cloned());
            }
        }

        // Cache stale — refresh.
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT schema_name, org_id, workspace_id FROM public.workspaces WHERE status = 'ACTIVE'",
        )
        .fetch_all(&self.pool)
        .await?;

        let schemas: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        let org_ws_map: HashMap<String, (String, String)> = rows
            .into_iter()
            .map(|(schema, org, ws)| (schema, (org, ws)))
            .collect();

        let result = org_ws_map.get(schema_name).cloned();

        let mut cache = self.cache.write().await;
        cache.schemas = schemas;
        cache.org_ws_map = org_ws_map;
        cache.fetched_at = Instant::now();

        Ok(result)
    }
}

impl SchemaProvider for SchemaRegistry {
    async fn get_active_schemas(&self) -> Result<Vec<String>, sqlx::Error> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if cache.fetched_at.elapsed() < self.ttl && !cache.schemas.is_empty() {
                return Ok(cache.schemas.clone());
            }
        }

        // Refresh — fetch org_id and workspace_id too so get_org_ws can serve
        // from the same cache without a second query.
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT schema_name, org_id, workspace_id FROM public.workspaces WHERE status = 'ACTIVE'",
        )
        .fetch_all(&self.pool)
        .await?;

        let schemas: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        let org_ws_map: HashMap<String, (String, String)> = rows
            .into_iter()
            .map(|(schema, org, ws)| (schema, (org, ws)))
            .collect();

        let mut cache = self.cache.write().await;
        cache.schemas = schemas.clone();
        cache.org_ws_map = org_ws_map;
        cache.fetched_at = Instant::now();

        Ok(schemas)
    }

    async fn get_org_ws(&self, schema_name: &str) -> Option<(String, String)> {
        // Delegate to the inherent method which handles cache + fallback DB query.
        // Errors (DB failures) are treated as "not found" so callers can proceed
        // with degraded callback URLs rather than hard-failing.
        SchemaRegistry::get_org_ws(self, schema_name)
            .await
            .unwrap_or(None)
    }
}

/// Blanket impl so `Arc<S>` can be used wherever `S: SchemaProvider` is expected.
/// This lets the poller pass its `Arc<S>` directly into spawned tasks.
impl<S: SchemaProvider> SchemaProvider for Arc<S> {
    async fn get_active_schemas(&self) -> Result<Vec<String>, sqlx::Error> {
        (**self).get_active_schemas().await
    }

    async fn get_org_ws(&self, schema_name: &str) -> Option<(String, String)> {
        (**self).get_org_ws(schema_name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_slugs() {
        for s in ["acme", "acme-corp", "team-1", "a", "ab-cd-ef", "x1y2z3"] {
            assert!(validate_slug(s), "expected '{s}' to be valid");
        }
    }

    #[test]
    fn rejects_malformed_slugs() {
        for s in [
            "",
            "-acme",
            "acme-",
            "Acme",      // uppercase
            "acme_corp", // underscore
            "acme corp", // space
            "acme.corp", // dot
            &"a".repeat(MAX_SLUG_LEN + 1),
        ] {
            assert!(!validate_slug(s), "expected '{s}' to be invalid");
        }
    }
}
