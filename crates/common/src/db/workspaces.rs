use crate::db::jobs::register_pg_cron_conn;
use crate::db::scoped::scoped_transaction;
use crate::db::tbl;
use crate::models::endpoint::EndpointType;
use crate::models::job::TriggerType;
use crate::models::workspace::Workspace;
use crate::tenant::validate_schema_name;
use sqlx::PgPool;

const WORKSPACE_SCHEMA_V1: &str = include_str!("../../migrations/workspace_v1.sql");

/// Endpoint name invokr installs in every workspace for its dogfooded reaper.
/// The reaper is an `INTERNAL` CRON job whose ticks materialize executions
/// into the workspace's own `executions` table — see `worker::dispatcher::internal`.
const REAPER_ENDPOINT_NAME: &str = "invokr.reaper";

pub async fn create(
    pool: &PgPool,
    org_id: &str,
    name: &str,
    slug: &str,
    schema_name: &str,
    reaper_cron_expression: &str,
) -> Result<Workspace, sqlx::Error> {
    assert!(
        validate_schema_name(schema_name),
        "Invalid schema name: {}",
        schema_name
    );

    let workspace = sqlx::query_as::<_, Workspace>(
        "INSERT INTO public.workspaces (org_id, name, slug, schema_name)
         VALUES ($1, $2, $3, $4)
         RETURNING *",
    )
    .bind(org_id)
    .bind(name)
    .bind(slug)
    .bind(schema_name)
    .fetch_one(pool)
    .await?;

    // Create the schema and apply workspace DDL
    provision_schema(pool, schema_name, "").await?;

    // Install invokr's own dogfooded reaper into this workspace. Done as part
    // of provisioning rather than from a background loop, so a freshly-created
    // workspace has its reaper job ready by the time `create` returns. If this
    // fails the workspace row exists but schema_version stays unset, marking
    // it as half-provisioned for the operator to investigate.
    provision_reaper(pool, schema_name, "", reaper_cron_expression).await?;

    // Update schema_version
    sqlx::query("UPDATE public.workspaces SET schema_version = 1 WHERE workspace_id = $1")
        .bind(&workspace.workspace_id)
        .execute(pool)
        .await?;

    // Re-fetch to get updated schema_version
    sqlx::query_as::<_, Workspace>("SELECT * FROM public.workspaces WHERE workspace_id = $1")
        .bind(&workspace.workspace_id)
        .fetch_one(pool)
        .await
}

pub async fn get(pool: &PgPool, workspace_id: &str) -> Result<Option<Workspace>, sqlx::Error> {
    sqlx::query_as::<_, Workspace>("SELECT * FROM public.workspaces WHERE workspace_id = $1")
        .bind(workspace_id)
        .fetch_optional(pool)
        .await
}

/// Resolve a workspace by its org reference and workspace reference, where each
/// reference may be the UUID id (`org_id` / `workspace_id`) or the human-readable
/// `slug`. Exact id matches are preferred over slug matches.
pub async fn get_by_org_and_id(
    pool: &PgPool,
    org_ref: &str,
    workspace_ref: &str,
) -> Result<Option<Workspace>, sqlx::Error> {
    sqlx::query_as::<_, Workspace>(
        "SELECT w.* FROM public.workspaces w
         JOIN public.organizations o ON o.org_id = w.org_id
         WHERE (o.org_id = $1 OR o.slug = $1)
           AND (w.workspace_id = $2 OR w.slug = $2)
         ORDER BY (o.org_id = $1) DESC, (w.workspace_id = $2) DESC
         LIMIT 1",
    )
    .bind(org_ref)
    .bind(workspace_ref)
    .fetch_optional(pool)
    .await
}

/// List active workspaces for an org addressed by its `org_id` or its `slug`.
pub async fn list_for_org(pool: &PgPool, org_ref: &str) -> Result<Vec<Workspace>, sqlx::Error> {
    sqlx::query_as::<_, Workspace>(
        "SELECT w.* FROM public.workspaces w
         JOIN public.organizations o ON o.org_id = w.org_id
         WHERE (o.org_id = $1 OR o.slug = $1) AND w.status = 'ACTIVE'
         ORDER BY w.created_at DESC",
    )
    .bind(org_ref)
    .fetch_all(pool)
    .await
}

/// Resolve the tenant schema name for an org/workspace pair, where each side may
/// be addressed by id or slug. Exact id matches are preferred.
pub async fn resolve_schema(
    pool: &PgPool,
    org_ref: &str,
    workspace_ref: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT w.schema_name FROM public.workspaces w
         JOIN public.organizations o ON o.org_id = w.org_id
         WHERE (o.org_id = $1 OR o.slug = $1)
           AND (w.workspace_id = $2 OR w.slug = $2)
           AND w.status = 'ACTIVE'
         ORDER BY (o.org_id = $1) DESC, (w.workspace_id = $2) DESC
         LIMIT 1",
    )
    .bind(org_ref)
    .bind(workspace_ref)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

pub async fn provision_schema(
    pool: &PgPool,
    schema_name: &str,
    table_prefix: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(&format!("CREATE SCHEMA IF NOT EXISTS \"{}\"", schema_name))
        .execute(pool)
        .await?;

    let mut conn = crate::db::scoped::scoped_connection(pool, schema_name).await?;

    // `table_prefix` is used as-is: callers pass the full prefix including any trailing
    // separator (e.g. "invokr_"), matching the read side (`tbl(prefix, name)` =
    // `{prefix}{name}`). Do NOT append an underscore here, or provisioned tables
    // (`invokr__endpoints`) won't match the names queried at runtime (`invokr_endpoints`).
    let ddl = WORKSPACE_SCHEMA_V1.replace("{p}", table_prefix);
    for stmt in ddl.split(';') {
        let stmt = stmt.trim();
        if !stmt.is_empty() {
            sqlx::query(stmt).execute(&mut *conn).await?;
        }
    }

    sqlx::query("SET search_path TO public")
        .execute(&mut *conn)
        .await?;

    Ok(())
}

/// Install the dogfooded reaper into a workspace: an `INTERNAL` endpoint, a
/// CRON job firing on `cron_expression`, and the matching pg_cron entry that
/// materializes one execution per tick. All three run inside the same scoped
/// transaction so a failure at any step rolls the whole provisioning back — we
/// never commit a job row without its pg_cron schedule (which would leave a
/// phantom row that never fires).
///
/// Idempotent: a workspace that already has an active reaper job is left as it
/// is. Library mode calls this from `provision_workspace`, which embedders run
/// at every startup, and the same call installs a reaper into a workspace
/// provisioned before library mode had one.
///
/// `cron_expression` is the caller-supplied schedule (typically from
/// `AppConfig::reaper::cron_expression`); it is validated as a 5-field
/// PgCronExpr at config-load time, so this function trusts it as a literal.
/// `table_prefix` is the workspace's table prefix (`""` for the API's
/// schema-per-workspace deployment, a real prefix like `"sched_"` for library
/// mode); it must match how the tables were provisioned, or the endpoint/job
/// inserts and the pg_cron command would target the wrong tables.
pub async fn provision_reaper(
    pool: &PgPool,
    schema_name: &str,
    table_prefix: &str,
    cron_expression: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = scoped_transaction(pool, schema_name).await?;

    // Serialise concurrent provisioners of one workspace (several instances
    // starting at once); otherwise each could see no reaper job and install its
    // own. Released when the transaction ends.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!(
            "{REAPER_ENDPOINT_NAME}:{schema_name}:{table_prefix}"
        ))
        .execute(&mut *tx)
        .await?;

    let te = tbl(table_prefix, "endpoints");
    let tj = tbl(table_prefix, "jobs");
    let reaper_spec = serde_json::json!({ "task": "reaper" });

    sqlx::query(&format!(
        "INSERT INTO {te} (name, endpoint_type, spec) VALUES ($1, $2, $3) \
         ON CONFLICT (name) DO NOTHING"
    ))
    .bind(REAPER_ENDPOINT_NAME)
    .bind(EndpointType::INTERNAL.to_string())
    .bind(&reaper_spec)
    .execute(&mut *tx)
    .await?;

    let installed: Option<String> = sqlx::query_scalar(&format!(
        "SELECT job_id FROM {tj} \
         WHERE endpoint = $1 AND trigger_type = $2 AND status = 'ACTIVE' LIMIT 1"
    ))
    .bind(REAPER_ENDPOINT_NAME)
    .bind(TriggerType::CRON.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    if installed.is_some() {
        return tx.commit().await;
    }

    let (job_id,): (String,) = sqlx::query_as(&format!(
        "INSERT INTO {tj} ( \
            endpoint, endpoint_type, trigger_type, \
            cron_expression, cron_timezone, cron_next_run_at \
         ) VALUES ($1, $2, $3, $4, 'UTC', now()) \
         RETURNING job_id"
    ))
    .bind(REAPER_ENDPOINT_NAME)
    .bind(EndpointType::INTERNAL.to_string())
    .bind(TriggerType::CRON.as_str())
    .bind(cron_expression)
    .fetch_one(&mut *tx)
    .await?;

    register_pg_cron_conn(&mut tx, table_prefix, schema_name, &job_id, cron_expression).await?;

    tx.commit().await?;

    Ok(())
}
