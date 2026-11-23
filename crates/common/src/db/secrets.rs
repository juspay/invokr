use crate::{
    db::{jobs::escape_like, tbl, DbContext},
    models::secret::Secret,
};

pub async fn create(
    db: &mut DbContext<'_>,
    name: &str,
    encrypted_value: &[u8],
) -> Result<Secret, sqlx::Error> {
    let t = tbl(db.prefix, "secrets");
    sqlx::query_as::<_, Secret>(&format!(
        "INSERT INTO {t} (name, encrypted_value) VALUES ($1, $2)
         RETURNING name, encrypted_value, created_at, updated_at"
    ))
    .bind(name)
    .bind(encrypted_value)
    .fetch_one(&mut *db.conn)
    .await
}

pub async fn get(db: &mut DbContext<'_>, name: &str) -> Result<Option<Secret>, sqlx::Error> {
    let t = tbl(db.prefix, "secrets");
    sqlx::query_as::<_, Secret>(&format!(
        "SELECT name, encrypted_value, created_at, updated_at FROM {t} WHERE name = $1"
    ))
    .bind(name)
    .fetch_optional(&mut *db.conn)
    .await
}

pub async fn list(
    db: &mut DbContext<'_>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<Vec<Secret>, sqlx::Error> {
    let t = tbl(db.prefix, "secrets");
    match cursor {
        Some(c) => {
            sqlx::query_as::<_, Secret>(&format!(
                "SELECT name, encrypted_value, created_at, updated_at FROM {t}
                 WHERE name > $1 ORDER BY name ASC LIMIT $2"
            ))
            .bind(c)
            .bind(limit)
            .fetch_all(&mut *db.conn)
            .await
        }
        None => {
            sqlx::query_as::<_, Secret>(&format!(
                "SELECT name, encrypted_value, created_at, updated_at FROM {t}
                 ORDER BY name ASC LIMIT $1"
            ))
            .bind(limit)
            .fetch_all(&mut *db.conn)
            .await
        }
    }
}

pub async fn update(
    db: &mut DbContext<'_>,
    name: &str,
    encrypted_value: &[u8],
) -> Result<Option<Secret>, sqlx::Error> {
    let t = tbl(db.prefix, "secrets");
    sqlx::query_as::<_, Secret>(&format!(
        "UPDATE {t} SET encrypted_value = $2, updated_at = now()
         WHERE name = $1
         RETURNING name, encrypted_value, created_at, updated_at"
    ))
    .bind(name)
    .bind(encrypted_value)
    .fetch_optional(&mut *db.conn)
    .await
}

pub async fn delete(db: &mut DbContext<'_>, name: &str) -> Result<bool, sqlx::Error> {
    let t = tbl(db.prefix, "secrets");
    let result = sqlx::query(&format!("DELETE FROM {t} WHERE name = $1"))
        .bind(name)
        .execute(&mut *db.conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn has_dependent_endpoints(
    db: &mut DbContext<'_>,
    name: &str,
) -> Result<bool, sqlx::Error> {
    let te = tbl(db.prefix, "endpoints");
    // spec::TEXT is JSON, so match the JSON-escaped name with LIKE wildcards escaped.
    let json = serde_json::Value::from(name).to_string();
    let pattern = format!("%{{{{secret.{}}}}}%", escape_like(&json[1..json.len() - 1]));
    let row: (i64,) = sqlx::query_as(&format!(
        "SELECT COUNT(*) FROM {te} WHERE spec::TEXT LIKE $1 ESCAPE '\\'"
    ))
    .bind(pattern)
    .fetch_one(&mut *db.conn)
    .await?;
    Ok(row.0 > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::pg_test::{drop_workspace, temp_workspace};
    use crate::db::scoped::scoped_connection;

    #[tokio::test]
    #[ignore = "requires Postgres: docker compose up -d postgres"]
    async fn has_dependent_endpoints_matches_secret_names_literally() {
        let (pool, schema) = temp_workspace().await;
        let mut conn = scoped_connection(&pool, &schema).await.unwrap();
        let spec = serde_json::json!({
            "url": "https://example.com",
            "headers": {
                "X-Plain": "{{secret.axb}}",
                "X-Quote": "{{secret.a\"b}}",
                "X-Slash": "{{secret.c\\d}}",
            },
        });
        sqlx::query(
            "INSERT INTO endpoints (name, endpoint_type, spec) VALUES ('ep', 'HTTP', $1::jsonb)",
        )
        .bind(spec.to_string())
        .execute(&mut *conn)
        .await
        .unwrap();

        let mut db = DbContext::new(&mut conn, "");
        let mut referenced = Vec::new();
        for name in ["axb", "a_b", "a%", "a\"b", "c\\d"] {
            referenced.push((name, has_dependent_endpoints(&mut db, name).await.unwrap()));
        }
        drop(conn);
        drop_workspace(&pool, &schema).await;

        assert_eq!(
            referenced,
            [
                ("axb", true),
                ("a_b", false),
                ("a%", false),
                ("a\"b", true),
                ("c\\d", true),
            ]
        );
    }
}
