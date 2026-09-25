//! Schema migrations for the `public` control plane, compiled into the binary.
//!
//! `sqlx::migrate!` reads `migrations/` at compile time and embeds each file's
//! SQL as a string literal, so the migrations travel inside the binary rather
//! than beside it. There is one version number — the image tag — and no way to
//! deploy code without the migrations it expects.
//!
//! This covers the control plane only. Per-workspace data tables are created at
//! runtime from `crates/common/migrations/workspace_v1.sql` when a workspace is
//! provisioned; see [`crate::db::workspaces`]. That file is deliberately not
//! part of this set.

use sqlx::migrate::{Migrate, Migrator};
use sqlx::PgPool;
use std::collections::HashSet;

/// The embedded migration set. The path is relative to this crate's manifest
/// directory, matching the existing `include_str!` in `db::workspaces`.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Apply every pending migration.
///
/// sqlx takes a Postgres advisory lock for the duration, so concurrent runners
/// serialise rather than racing, and records each migration in
/// `_sqlx_migrations` inside the same transaction as the migration itself.
pub async fn run(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await?;
    tracing::info!("migrations applied");
    Ok(())
}

/// Print the SQL a real run would execute, without touching the database.
///
/// Emits each pending migration wrapped in a transaction, followed by the
/// `_sqlx_migrations` bookkeeping row sqlx would write. That makes the output
/// reviewable by a DBA, and is how an existing database whose schema was
/// applied by hand gets baselined: keep the INSERTs, discard the DDL.
pub async fn dry_run(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    conn.ensure_migrations_table().await?;

    let applied: HashSet<i64> = conn
        .list_applied_migrations()
        .await?
        .into_iter()
        .map(|m| m.version)
        .collect();
    drop(conn);

    let mut pending = 0usize;
    for migration in MIGRATOR.iter() {
        if migration.migration_type.is_down_migration() || applied.contains(&migration.version) {
            continue;
        }
        pending += 1;

        let checksum: String = migration
            .checksum
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        // Postgres string literals escape a single quote by doubling it.
        let description = migration.description.replace('\'', "''");

        println!(
            "-- migration {} ({})",
            migration.version, migration.description
        );
        println!("BEGIN;");
        println!("{}", migration.sql.trim_end());
        // execution_time is -1 because sqlx cannot measure it inside the
        // transaction it is about to commit; the value exists for debugging.
        println!(
            "INSERT INTO _sqlx_migrations \
             (version, description, success, checksum, execution_time) \
             VALUES ({}, '{}', TRUE, '\\x{}'::bytea, -1);",
            migration.version, description, checksum
        );
        println!("COMMIT;\n");
    }

    if pending == 0 {
        println!("-- no pending migrations");
    }
    // stderr, so stdout stays a clean SQL stream that can be piped into psql.
    eprintln!("dry run: {pending} pending migration(s), nothing applied");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the compile-time path: if `migrations/` moves or a filename stops
    /// parsing as `<version>_<description>.sql`, this fails rather than the set
    /// silently shrinking.
    #[test]
    fn migrator_embeds_the_migration_set() {
        let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        assert!(
            !versions.is_empty(),
            "no migrations embedded — check the sqlx::migrate! path"
        );
        let mut sorted = versions.clone();
        sorted.sort_unstable();
        assert_eq!(versions, sorted, "migrations must be ordered by version");
    }

    /// Strip `--` line comments. Enough for the placeholder check below, which
    /// only needs to distinguish executable SQL from prose -- migrations quote
    /// `workspace_v1.sql` in comments, placeholders included.
    fn executable_sql(sql: &str) -> String {
        sql.lines()
            .map(|l| l.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `workspace_v1.sql` is a runtime template applied per workspace, not a
    /// versioned migration. If it is ever moved into `migrations/` it would be
    /// applied once against `public` with its `{p}` placeholder unsubstituted.
    #[test]
    fn workspace_template_is_not_part_of_the_set() {
        for migration in MIGRATOR.iter() {
            assert!(
                !migration.description.contains("workspace_v1"),
                "workspace_v1.sql must not be a versioned migration"
            );
            assert!(
                !executable_sql(&migration.sql).contains("{p}"),
                "migration {} executes an unsubstituted {{p}} placeholder",
                migration.version
            );
        }
    }
}
