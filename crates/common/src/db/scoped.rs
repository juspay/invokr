use crate::tenant::check_schema_name;
use sqlx::PgPool;

/// Acquire a connection with search_path set to the workspace schema.
pub async fn scoped_connection(
    pool: &PgPool,
    schema_name: &str,
) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, sqlx::Error> {
    check_schema_name(schema_name)?;
    let mut conn = pool.acquire().await?;
    // Strict workspace scope, no `public` fallback: a missing schema must error
    // rather than silently read public (control tables use `public.*` directly).
    let set_path = format!("SET search_path TO \"{}\"", schema_name);
    sqlx::query(&set_path).execute(&mut *conn).await?;
    Ok(conn)
}

/// Begin a transaction with search_path set to the workspace schema.
pub async fn scoped_transaction<'a>(
    pool: &'a PgPool,
    schema_name: &str,
) -> Result<sqlx::Transaction<'a, sqlx::Postgres>, sqlx::Error> {
    check_schema_name(schema_name)?;
    let mut tx = pool.begin().await?;
    // Strict workspace scope, no `public` fallback (see `scoped_connection`).
    let set_path = format!("SET search_path TO \"{}\"", schema_name);
    sqlx::query(&set_path).execute(&mut *tx).await?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{is_invalid_identifier, unreachable_pool};

    const HOSTILE: &str = "ws\"; DROP SCHEMA public CASCADE; --";

    #[tokio::test]
    async fn scoped_connection_returns_error_for_unsafe_schema_name() {
        let Err(err) = scoped_connection(&unreachable_pool(), HOSTILE).await else {
            panic!("unsafe schema name must be rejected");
        };
        assert!(is_invalid_identifier(&err), "got {err:?}");
    }

    #[tokio::test]
    async fn scoped_transaction_returns_error_for_unsafe_schema_name() {
        let Err(err) = scoped_transaction(&unreachable_pool(), HOSTILE).await else {
            panic!("unsafe schema name must be rejected");
        };
        assert!(is_invalid_identifier(&err), "got {err:?}");
    }

    #[tokio::test]
    async fn scoped_connection_passes_safe_schema_name_through_to_db() {
        // The pool is unreachable, so getting past validation means a pool
        // error rather than an identifier rejection.
        let Err(err) = scoped_connection(&unreachable_pool(), "acme_billing").await else {
            panic!("unreachable pool cannot hand out a connection");
        };
        assert!(
            !is_invalid_identifier(&err),
            "safe name was rejected: {err:?}"
        );
    }
}
