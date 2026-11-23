pub mod attempts;
pub mod configs;
pub mod endpoints;
pub mod execution_logs;
pub mod executions;
pub mod jobs;
pub mod organizations;
pub mod payload_specs;
pub mod scoped;
pub mod secrets;
pub mod workspaces;

use crate::tenant::TablePrefix;
use sqlx::PgConnection;
use sqlx::PgPool;

/// Connect a `PgPool` with Invokr's logging policy: SQL statement logs are
/// demoted from sqlx's default DEBUG to TRACE so steady-state chatter (e.g.
/// the worker's poll queries) doesn't flood debug logs. Opt back in with
/// `RUST_LOG=sqlx::query=trace`.
pub async fn connect_pool(database_url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    use sqlx::ConnectOptions;

    let options = database_url
        .parse::<sqlx::postgres::PgConnectOptions>()?
        .log_statements(log::LevelFilter::Trace);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await
}

/// Bundles a mutable DB connection and its table prefix, eliminating the need
/// to thread two separate parameters through every DB function signature.
pub struct DbContext<'a> {
    pub conn: &'a mut PgConnection,
    pub prefix: &'a TablePrefix,
}

impl<'a> DbContext<'a> {
    pub fn new(conn: &'a mut PgConnection, prefix: &'a TablePrefix) -> Self {
        Self { conn, prefix }
    }
}

/// Build a (potentially prefixed) table name.
/// With prefix `sched_`, `tbl(prefix, "jobs")` → `"sched_jobs"`; with the
/// default (empty) prefix → `"jobs"`.
/// Taking a [`TablePrefix`] means the prefix spliced in here was validated.
#[inline]
pub fn tbl(prefix: &TablePrefix, name: &str) -> String {
    format!("{}{name}", prefix.as_str())
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::tenant::InvalidIdentifier;
    use sqlx::PgPool;
    use std::time::Duration;

    /// A pool aimed at a closed port. Anything that actually reaches the
    /// database fails with `PoolTimedOut`, so a test can tell "rejected before
    /// touching the DB" apart from "ran SQL".
    pub fn unreachable_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(100))
            .connect_lazy("postgres://invokr:invokr@127.0.0.1:1/invokr_db")
            .unwrap()
    }

    /// True when `err` is the up-front identifier rejection.
    pub fn is_invalid_identifier(err: &sqlx::Error) -> bool {
        matches!(err, sqlx::Error::Configuration(e) if e.is::<InvalidIdentifier>())
    }
}

#[cfg(test)]
pub(crate) mod pg_test {
    use sqlx::PgPool;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT_ID: AtomicU32 = AtomicU32::new(0);

    pub async fn temp_workspace() -> (PgPool, String) {
        let url = std::env::var("INVOKR_DATABASE_URL")
            .unwrap_or_else(|_| "postgresql://invokr:invokr@localhost:5434/invokr_db".into());
        let pool = PgPool::connect(&url).await.unwrap();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let schema = format!("invokr_test_{}_{id}", std::process::id());
        super::workspaces::provision_schema(&pool, &schema, &crate::tenant::TablePrefix::default())
            .await
            .unwrap();
        (pool, schema)
    }

    pub async fn drop_workspace(pool: &PgPool, schema: &str) {
        sqlx::query(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
            .execute(pool)
            .await
            .unwrap();
    }
}
