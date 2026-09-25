use async_trait::async_trait;
use chrono::{DateTime, Utc};
use invokr_common::{
    cache::{ConfigCache, SecretCache},
    db,
    db::DbContext,
    models::Execution,
    tenant::{validate_table_prefix, SchemaProvider},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::pipeline::PipelineContext;
use crate::poller;

/// How a job should be triggered.
#[derive(Serialize, Deserialize)]
pub enum JobTrigger {
    /// Fire immediately, create a QUEUED execution right away.
    Immediate,
    /// Fire at a specific future time.
    Delayed { run_at: DateTime<Utc> },
    /// Recurring CRON schedule.
    Cron {
        expression: String,
        timezone: String,
        starts_at: Option<DateTime<Utc>>,
        ends_at: Option<DateTime<Utc>>,
        first_run_at: DateTime<Utc>,
    },
}

/// Abstracts over library-mode (`InvokrLibraryClient`) and service-mode (`InvokrHttpClient`).
/// Switching between the two requires only env-var changes — no code changes at call sites.
#[async_trait]
pub trait InvokrClient: Send + Sync {
    async fn upsert_secret(
        &self,
        schema_name: &str,
        name: &str,
        plaintext: &str,
    ) -> anyhow::Result<()>;

    async fn delete_secret(&self, schema_name: &str, name: &str) -> anyhow::Result<()>;

    async fn register_endpoint(
        &self,
        schema_name: &str,
        name: &str,
        endpoint_type: &str,
        spec: serde_json::Value,
        retry_policy: Option<serde_json::Value>,
    ) -> anyhow::Result<()>;

    async fn delete_endpoint(&self, schema_name: &str, name: &str) -> anyhow::Result<()>;

    async fn create_job(
        &self,
        schema_name: &str,
        endpoint: &str,
        input: serde_json::Value,
        max_attempts: i64,
        trigger: JobTrigger,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<String>;

    /// Library mode: no-op — the caller's workspace template already provisioned the tables.
    /// Service mode: tells Invokr service to create the scheduler tables in its own DB.
    async fn provision_workspace(&self, schema_name: &str) -> anyhow::Result<()>;

    /// Cancel a job. For CRON jobs also unregisters the pg_cron schedule.
    /// For Immediate/Delayed jobs also cancels any PENDING/QUEUED executions.
    async fn cancel_job(&self, schema_name: &str, job_id: &str) -> anyhow::Result<()>;

    /// Fetch a single execution by ID. Returns None if not found.
    async fn get_execution(
        &self,
        schema_name: &str,
        execution_id: &str,
    ) -> anyhow::Result<Option<Execution>>;
}

/// Configuration for the background worker.
pub struct WorkerConfig {
    pub max_concurrent: usize,
    pub poll_interval_ms: u64,
    pub config_cache_ttl_sec: u64,
    pub secret_cache_ttl_sec: u64,
    pub shutdown_timeout_sec: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 50,
            poll_interval_ms: 200,
            config_cache_ttl_sec: 60,
            secret_cache_ttl_sec: 300,
            shutdown_timeout_sec: 30,
        }
    }
}

/// The public API for embedding Invokr in another application.
///
/// Holds a caller-provided `PgPool` and exposes job creation, endpoint
/// registration, and worker startup. The caller controls pool sizing.
#[derive(Clone)]
pub struct InvokrLibraryClient {
    pool: PgPool,
    ctx: Arc<PipelineContext>,
}

impl InvokrLibraryClient {
    /// `table_prefix` must include the trailing underscore (e.g. `"sched_"`); use `""` for no prefix.
    pub fn new(
        pool: PgPool,
        table_prefix: &str,
        encryption_key: &str,
        http_client: Option<Client>,
    ) -> anyhow::Result<Self> {
        if !validate_table_prefix(table_prefix) {
            anyhow::bail!(
                "table_prefix '{}' is invalid: only alphanumeric and underscore allowed",
                table_prefix
            );
        }

        let ctx = Arc::new(PipelineContext {
            pool: pool.clone(),
            http_client: http_client.unwrap_or_default(),
            config_cache: ConfigCache::new(60),
            secret_cache: SecretCache::new(300),
            encryption_key: encryption_key.to_string(),
            table_prefix: table_prefix.to_string(),
        });

        Ok(Self { pool, ctx })
    }

    /// Convenience constructor for callers that don't manage their own `PgPool`:
    /// builds an internal pool from `database_url` and `max_connections`.
    /// Use [`Self::pool`] to share the pool (e.g. with a `SchemaProvider`).
    /// Callers that need finer pool control should build a `PgPool` themselves
    /// and use [`Self::new`].
    pub async fn from_database_url(
        database_url: &str,
        max_connections: u32,
        table_prefix: &str,
        encryption_key: &str,
        http_client: Option<Client>,
    ) -> anyhow::Result<Self> {
        let pool = db::connect_pool(database_url, max_connections).await?;
        Self::new(pool, table_prefix, encryption_key, http_client)
    }

    /// The connection pool this client runs on.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Create a job in the given workspace schema and return the execution_id.
    pub async fn create_job(
        &self,
        schema_name: &str,
        endpoint: &str,
        input: serde_json::Value,
        max_attempts: i64,
        trigger: JobTrigger,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<String> {
        let prefix = self.ctx.table_prefix.as_str();
        let ikey = idempotency_key.unwrap_or("");

        let mut conn = db::scoped::scoped_connection(&self.pool, schema_name).await?;
        let mut db = DbContext::new(&mut conn, prefix);

        let ep = db::endpoints::get(&mut db, endpoint)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Endpoint '{}' not found in schema '{}'",
                    endpoint,
                    schema_name
                )
            })?;

        let execution_id = match trigger {
            JobTrigger::Immediate => {
                let result = db::jobs::create_immediate(
                    &mut db,
                    endpoint,
                    ep.endpoint_type.as_str(),
                    ikey,
                    Some(&input),
                    max_attempts,
                )
                .await?;
                result.execution_id
            }
            JobTrigger::Delayed { run_at } => {
                let result = db::jobs::create_delayed(
                    &mut db,
                    endpoint,
                    ep.endpoint_type.as_str(),
                    ikey,
                    Some(&input),
                    run_at,
                    max_attempts,
                )
                .await?;
                result.execution_id
            }
            JobTrigger::Cron {
                expression,
                timezone,
                starts_at,
                ends_at,
                first_run_at,
            } => {
                let job = db::jobs::create_cron(
                    &mut db,
                    endpoint,
                    ep.endpoint_type.as_str(),
                    idempotency_key,
                    Some(&input),
                    &expression,
                    &timezone,
                    starts_at,
                    ends_at,
                    first_run_at,
                )
                .await?;
                job.job_id
            }
        };

        Ok(execution_id)
    }

    /// Register (upsert) an endpoint in the given workspace schema.
    pub async fn register_endpoint(
        &self,
        schema_name: &str,
        name: &str,
        endpoint_type: &str,
        spec: serde_json::Value,
        retry_policy: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let prefix = self.ctx.table_prefix.as_str();
        let mut conn = db::scoped::scoped_connection(&self.pool, schema_name).await?;
        let mut db = DbContext::new(&mut conn, prefix);

        let existing = db::endpoints::get(&mut db, name).await?;
        if existing.is_none() {
            db::endpoints::create(
                &mut db,
                name,
                endpoint_type,
                None,
                None,
                &spec,
                retry_policy.as_ref(),
            )
            .await?;
        } else {
            db::endpoints::update(
                &mut db,
                name,
                Some(&spec),
                None,
                None,
                retry_policy.as_ref(),
            )
            .await?;
        }

        Ok(())
    }

    /// Delete an endpoint from the given workspace schema.
    pub async fn delete_endpoint(&self, schema_name: &str, name: &str) -> anyhow::Result<()> {
        let prefix = self.ctx.table_prefix.as_str();
        let mut conn = db::scoped::scoped_connection(&self.pool, schema_name).await?;
        let mut db = DbContext::new(&mut conn, prefix);
        db::endpoints::delete(&mut db, name).await?;
        Ok(())
    }

    /// Upsert a secret in the given workspace schema.
    /// The plaintext value is encrypted with Invokr's own encryption key before storage.
    pub async fn upsert_secret(
        &self,
        schema_name: &str,
        name: &str,
        plaintext: &str,
    ) -> anyhow::Result<()> {
        let prefix = self.ctx.table_prefix.as_str();
        let encrypted = invokr_common::crypto::encrypt(plaintext, &self.ctx.encryption_key)?;
        let mut conn = db::scoped::scoped_connection(&self.pool, schema_name).await?;
        let mut db = DbContext::new(&mut conn, prefix);
        if db::secrets::get(&mut db, name).await?.is_some() {
            db::secrets::update(&mut db, name, &encrypted).await?;
        } else {
            db::secrets::create(&mut db, name, &encrypted).await?;
        }
        Ok(())
    }

    /// Delete a secret from the given workspace schema. No-op if the secret does not exist.
    pub async fn delete_secret(&self, schema_name: &str, name: &str) -> anyhow::Result<()> {
        let prefix = self.ctx.table_prefix.as_str();
        let mut conn = db::scoped::scoped_connection(&self.pool, schema_name).await?;
        let mut db = DbContext::new(&mut conn, prefix);
        db::secrets::delete(&mut db, name).await?;
        Ok(())
    }

    /// Cancel a job and its pending executions.
    // `drop(db)` releases the scoped connection borrow before the pg_cron
    // unschedule runs on the pool; it is a borrow handoff, not a destructor.
    #[allow(clippy::drop_non_drop)]
    pub async fn cancel_job(&self, schema_name: &str, job_id: &str) -> anyhow::Result<()> {
        let prefix = self.ctx.table_prefix.as_str();
        let mut conn = db::scoped::scoped_connection(&self.pool, schema_name).await?;
        let mut db = DbContext::new(&mut conn, prefix);

        let job = db::jobs::get(&mut db, job_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Job '{}' not found", job_id))?;

        if job.trigger_type != "CRON" {
            db::executions::cancel_pending_for_job(&mut db, job_id).await?;
        }
        db::jobs::cancel(&mut db, job_id).await?;
        drop(db);
        if job.trigger_type == "CRON" {
            db::jobs::unregister_pg_cron(&self.pool, schema_name, job_id).await?;
        }
        Ok(())
    }

    /// Fetch a single execution by ID.
    pub async fn get_execution(
        &self,
        schema_name: &str,
        execution_id: &str,
    ) -> anyhow::Result<Option<Execution>> {
        let prefix = self.ctx.table_prefix.as_str();
        let mut conn = db::scoped::scoped_connection(&self.pool, schema_name).await?;
        let mut db = DbContext::new(&mut conn, prefix);
        Ok(db::executions::get(&mut db, execution_id).await?)
    }

    /// Start the background worker. Returns a [`WorkerHandle`] — call
    /// [`WorkerHandle::shutdown`] on shutdown, then await [`WorkerHandle::join`].
    ///
    /// Pass a `WorkerConfig` to control concurrency, poll interval, etc.
    pub fn start_worker<S: SchemaProvider>(
        &self,
        schema_provider: S,
        worker_config: WorkerConfig,
    ) -> WorkerHandle {
        let pool = self.pool.clone();
        let ctx = self.ctx.clone();

        // Build an AppConfig-compatible struct from the context + worker_config
        let config = build_app_config(&ctx, &worker_config);

        let cancel = CancellationToken::new();
        let join = {
            let cancel = cancel.clone();
            tokio::spawn(async move { poller::run(pool, config, schema_provider, cancel).await })
        };
        WorkerHandle { cancel, join }
    }
}

/// Handle to a running background worker. Owns the cancellation token and the
/// task handle so embedders don't need their own tokio-util dependency.
pub struct WorkerHandle {
    cancel: CancellationToken,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl WorkerHandle {
    /// Signal the worker to stop. Returns immediately; the worker finishes
    /// in-flight jobs (bounded by `WorkerConfig::shutdown_timeout_sec`).
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    /// Wait for the worker task to finish.
    pub async fn join(self) -> anyhow::Result<()> {
        self.join.await?
    }
}

/// Build an AppConfig from the PipelineContext and WorkerConfig for the poller.
fn build_app_config(ctx: &PipelineContext, wc: &WorkerConfig) -> invokr_common::config::AppConfig {
    use invokr_common::config::{
        AppConfig, CryptoEnv, DbEnv, MetricsEnv, MigrationMode, ReaperEnv, ServerEnv, ServerMode,
        WorkerEnv,
    };

    AppConfig {
        db: DbEnv {
            url: String::new(), // pool already created by caller
            pool_size: 0,
            table_prefix: ctx.table_prefix.clone(),
            migration_mode: MigrationMode::None,
        },
        server: ServerEnv {
            listen_addr: String::new(),
            api_key: String::new(),
            path_prefix: String::new(),
            mode: ServerMode::Api,
            dashboard_prefix: String::new(),
            dashboard_dist_dir: String::new(),
        },
        worker: WorkerEnv {
            max_concurrent: wc.max_concurrent,
            poll_interval_ms: wc.poll_interval_ms,
            config_cache_ttl_sec: wc.config_cache_ttl_sec,
            secret_cache_ttl_sec: wc.secret_cache_ttl_sec,
            shutdown_timeout_sec: wc.shutdown_timeout_sec,
        },
        crypto: CryptoEnv {
            encryption_key: ctx.encryption_key.clone(),
        },
        metrics: MetricsEnv { port: 0 },
        // Reaper schedule is consumed at workspace creation (API side), not by the
        // worker poller; the library worker just runs reaper ticks as they arrive.
        reaper: ReaperEnv {
            cron_expression: "*/15 * * * *".to_string(),
        },
    }
}

#[async_trait]
impl InvokrClient for InvokrLibraryClient {
    async fn upsert_secret(
        &self,
        schema_name: &str,
        name: &str,
        plaintext: &str,
    ) -> anyhow::Result<()> {
        InvokrLibraryClient::upsert_secret(self, schema_name, name, plaintext).await
    }

    async fn delete_secret(&self, schema_name: &str, name: &str) -> anyhow::Result<()> {
        InvokrLibraryClient::delete_secret(self, schema_name, name).await
    }

    async fn register_endpoint(
        &self,
        schema_name: &str,
        name: &str,
        endpoint_type: &str,
        spec: serde_json::Value,
        retry_policy: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        InvokrLibraryClient::register_endpoint(
            self,
            schema_name,
            name,
            endpoint_type,
            spec,
            retry_policy,
        )
        .await
    }

    async fn delete_endpoint(&self, schema_name: &str, name: &str) -> anyhow::Result<()> {
        InvokrLibraryClient::delete_endpoint(self, schema_name, name).await
    }

    async fn create_job(
        &self,
        schema_name: &str,
        endpoint: &str,
        input: serde_json::Value,
        max_attempts: i64,
        trigger: JobTrigger,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<String> {
        InvokrLibraryClient::create_job(
            self,
            schema_name,
            endpoint,
            input,
            max_attempts,
            trigger,
            idempotency_key,
        )
        .await
    }

    async fn provision_workspace(&self, schema_name: &str) -> anyhow::Result<()> {
        Ok(
            db::workspaces::provision_schema(&self.pool, schema_name, &self.ctx.table_prefix)
                .await?,
        )
    }

    async fn cancel_job(&self, schema_name: &str, job_id: &str) -> anyhow::Result<()> {
        InvokrLibraryClient::cancel_job(self, schema_name, job_id).await
    }

    async fn get_execution(
        &self,
        schema_name: &str,
        execution_id: &str,
    ) -> anyhow::Result<Option<Execution>> {
        InvokrLibraryClient::get_execution(self, schema_name, execution_id).await
    }
}

/// HTTP client for Invokr-as-a-service mode. Implements `InvokrClient` identically
/// to `InvokrLibraryClient` so all call sites are transparent to the deployment mode.
///
/// Workspace routing: each request sends `x-org-id` and `x-workspace-id` (= schema_name)
/// headers. Invokr resolves the workspace by slug, which requires `resolve_schema` in Invokr
/// to accept slug as well as workspace_id UUID (see `db/workspaces.rs`).
pub struct InvokrHttpClient {
    base_url: String,
    api_key: String,
    /// Invokr org_id this client operates under. Set via `INVOKR_ORG_ID` env var.
    org_id: String,
    http_client: Client,
}

impl InvokrHttpClient {
    pub fn new(base_url: String, api_key: String, org_id: String) -> Self {
        Self {
            base_url,
            api_key,
            org_id,
            http_client: Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v1{}", self.base_url.trim_end_matches('/'), path)
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("Authorization", format!("Bearer {}", self.api_key))
    }

    fn with_workspace(
        &self,
        req: reqwest::RequestBuilder,
        schema_name: &str,
    ) -> reqwest::RequestBuilder {
        req.header("x-org-id", &self.org_id)
            .header("x-workspace-id", schema_name)
    }

    async fn check(resp: reqwest::Response, ctx: &str) -> anyhow::Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            Ok(resp)
        } else {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("{ctx} HTTP {status}: {body}");
        }
    }
}

#[async_trait]
impl InvokrClient for InvokrHttpClient {
    async fn upsert_secret(
        &self,
        schema_name: &str,
        name: &str,
        plaintext: &str,
    ) -> anyhow::Result<()> {
        // Try create; on 409 (already exists) fall through to update.
        let resp = self
            .authed(self.with_workspace(self.http_client.post(self.url("/secrets")), schema_name))
            .json(&serde_json::json!({ "name": name, "value": plaintext }))
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        if status.as_u16() != 409 {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("upsert_secret create HTTP {status}: {body}");
        }
        // 409 — secret exists, update it.
        let resp = self
            .authed(self.with_workspace(
                self.http_client.put(self.url(&format!("/secrets/{name}"))),
                schema_name,
            ))
            .json(&serde_json::json!({ "value": plaintext }))
            .send()
            .await?;
        Self::check(resp, "upsert_secret update").await?;
        Ok(())
    }

    async fn delete_secret(&self, schema_name: &str, name: &str) -> anyhow::Result<()> {
        let resp = self
            .authed(
                self.with_workspace(
                    self.http_client
                        .delete(self.url(&format!("/secrets/{name}"))),
                    schema_name,
                ),
            )
            .send()
            .await?;
        Self::check(resp, "delete_secret").await?;
        Ok(())
    }

    async fn register_endpoint(
        &self,
        schema_name: &str,
        name: &str,
        endpoint_type: &str,
        spec: serde_json::Value,
        retry_policy: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        // Try update first; on 404 (doesn't exist yet) fall through to create.
        let resp = self
            .authed(
                self.with_workspace(
                    self.http_client
                        .put(self.url(&format!("/endpoints/{name}"))),
                    schema_name,
                ),
            )
            .json(&serde_json::json!({ "spec": spec, "retry_policy": retry_policy }))
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        if status.as_u16() != 404 {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("register_endpoint update HTTP {status}: {body}");
        }
        // 404 — endpoint doesn't exist, create it.
        let resp = self
            .authed(self.with_workspace(self.http_client.post(self.url("/endpoints")), schema_name))
            .json(&serde_json::json!({
                "name": name,
                "type": endpoint_type,
                "spec": spec,
                "retry_policy": retry_policy,
            }))
            .send()
            .await?;
        Self::check(resp, "register_endpoint create").await?;
        Ok(())
    }

    async fn delete_endpoint(&self, schema_name: &str, name: &str) -> anyhow::Result<()> {
        let resp = self
            .authed(
                self.with_workspace(
                    self.http_client
                        .delete(self.url(&format!("/endpoints/{name}"))),
                    schema_name,
                ),
            )
            .send()
            .await?;
        Self::check(resp, "delete_endpoint").await?;
        Ok(())
    }

    async fn create_job(
        &self,
        schema_name: &str,
        endpoint: &str,
        input: serde_json::Value,
        max_attempts: i64,
        trigger: JobTrigger,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<String> {
        let is_cron = matches!(trigger, JobTrigger::Cron { .. });

        // Build trigger-specific fields as a flat JSON object.
        let (trigger_str, extra) = match trigger {
            JobTrigger::Immediate => ("IMMEDIATE", serde_json::json!({})),
            JobTrigger::Delayed { run_at } => ("DELAYED", serde_json::json!({ "run_at": run_at })),
            JobTrigger::Cron {
                expression,
                timezone,
                starts_at,
                ends_at,
                ..
            } => (
                "CRON",
                serde_json::json!({
                    "cron": expression,
                    "timezone": timezone,
                    "starts_at": starts_at,
                    "ends_at": ends_at,
                }),
            ),
        };

        let mut body = serde_json::json!({
            "endpoint": endpoint,
            "input": input,
            "trigger": trigger_str,
        });
        if max_attempts > 0 {
            body["max_attempts"] = serde_json::json!(max_attempts);
        }
        if let Some(key) = idempotency_key {
            body["idempotency_key"] = serde_json::json!(key);
        }
        if let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
            for (k, v) in extra_obj {
                body_obj.insert(k.clone(), v.clone());
            }
        }

        let resp = self
            .authed(self.with_workspace(self.http_client.post(self.url("/jobs")), schema_name))
            .json(&body)
            .send()
            .await?;
        let resp = Self::check(resp, "create_job").await?;
        let json: serde_json::Value = resp.json().await?;

        if is_cron {
            json["data"]["job_id"]
                .as_str()
                .map(String::from)
                .ok_or_else(|| anyhow::anyhow!("create_job: response missing 'data.job_id'"))
        } else {
            json["data"]["execution"]["execution_id"]
                .as_str()
                .map(String::from)
                .ok_or_else(|| {
                    anyhow::anyhow!("create_job: response missing 'data.execution.execution_id'")
                })
        }
    }

    async fn provision_workspace(&self, schema_name: &str) -> anyhow::Result<()> {
        // Register workspace in Invokr so it can resolve x-workspace-id = schema_name.
        // The org must already exist (created by the operator, org_id set via INVOKR_ORG_ID).
        // Workspace slug = schema_name; Invokr resolve_schema accepts slug OR uuid.
        let resp = self
            .authed(
                self.http_client
                    .post(self.url(&format!("/orgs/{}/workspaces", self.org_id))),
            )
            .json(&serde_json::json!({ "name": schema_name, "slug": schema_name }))
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() || status.as_u16() == 409 {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("provision_workspace HTTP {status}: {body}");
    }

    async fn cancel_job(&self, schema_name: &str, job_id: &str) -> anyhow::Result<()> {
        let resp = self
            .authed(
                self.with_workspace(
                    self.http_client
                        .post(self.url(&format!("/jobs/{job_id}/cancel"))),
                    schema_name,
                ),
            )
            .send()
            .await?;
        Self::check(resp, "cancel_job").await?;
        Ok(())
    }

    async fn get_execution(
        &self,
        schema_name: &str,
        execution_id: &str,
    ) -> anyhow::Result<Option<Execution>> {
        let resp = self
            .authed(
                self.with_workspace(
                    self.http_client
                        .get(self.url(&format!("/executions/{execution_id}"))),
                    schema_name,
                ),
            )
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let resp = Self::check(resp, "get_execution").await?;
        let json: serde_json::Value = resp.json().await?;
        let execution: Execution = serde_json::from_value(json["data"].clone())?;
        Ok(Some(execution))
    }
}
