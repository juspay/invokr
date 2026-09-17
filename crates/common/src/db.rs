pub mod attempts;
pub mod configs;
pub mod endpoints;
pub mod execution_logs;
pub mod executions;
pub mod jobs;
pub mod organizations;
pub mod payload_specs;
pub mod polls;
pub mod scoped;
pub mod secrets;
pub mod workspaces;

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
    pub prefix: &'a str,
}

impl<'a> DbContext<'a> {
    pub fn new(conn: &'a mut PgConnection, prefix: &'a str) -> Self {
        Self { conn, prefix }
    }
}

/// Build a (potentially prefixed) table name.
/// `tbl("sched_", "jobs")` → `"sched_jobs"`, `tbl("", "jobs")` → `"jobs"`.
#[inline]
pub fn tbl(prefix: &str, name: &str) -> String {
    format!("{prefix}{name}")
}
