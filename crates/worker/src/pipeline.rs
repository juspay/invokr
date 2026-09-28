use chrono::Utc;
use invokr_common::{
    cache::{ConfigCache, SecretCache},
    db,
    db::DbContext,
    metrics as m, template,
};
use reqwest::Client;
use sqlx::PgPool;
use std::collections::HashMap;

use crate::backoff;
use crate::dispatcher::{self, DispatchResult};

pub struct PipelineContext {
    pub pool: PgPool,
    pub http_client: Client,
    pub config_cache: ConfigCache,
    pub secret_cache: SecretCache,
    pub encryption_key: String,
    pub table_prefix: String,
    /// Base URL for the Invokr API (e.g. "https://invokr.example"). Used to
    /// construct callback URLs embedded in long-running-job dispatch bodies.
    /// Empty string means callback URLs will also be empty (template resolves
    /// to empty strings, which is debuggable).
    pub api_base_url: String,
    /// URL path prefix for the API (e.g. "" or "/invokr"). Matches
    /// `AppConfig::server::path_prefix`.
    pub path_prefix: String,
}

// The execution row and its job/endpoint columns are passed through
// individually rather than re-fetched here; bundling them would just move
// the same list into a struct the poller has to fill in.
#[allow(clippy::too_many_arguments)]
pub async fn process_execution(
    ctx: &PipelineContext,
    db: &mut DbContext<'_>,
    schema_name: &str,
    execution_id: &str,
    idempotency_key: &str,
    job_id: &str,
    endpoint_name: &str,
    endpoint_type: &str,
    input: Option<&serde_json::Value>,
    attempt_count: i64,
    max_attempts: i64,
    org_id: &str,
    workspace_id: &str,
    // Per-job async override: max wait ms from the job row; None = use endpoint default.
    async_max_wait_ms: Option<i64>,
    // Per-job async override: max polls from the job row; None = use endpoint default.
    async_max_polls: Option<i32>,
) {
    let started_at = Utc::now();

    // 1. Load endpoint
    let endpoint = match db::endpoints::get(db, endpoint_name).await {
        Ok(Some(ep)) => ep,
        Ok(None) => {
            tracing::error!(execution_id, "Endpoint not found: {}", endpoint_name);
            let _ = db::executions::complete_failed(db, execution_id).await;
            log_execution(
                db,
                execution_id,
                attempt_count,
                "ERROR",
                &format!("Endpoint not found: {}", endpoint_name),
            )
            .await;
            return;
        }
        Err(e) => {
            tracing::error!(execution_id, "Failed to load endpoint: {}", e);
            let _ = db::executions::complete_failed(db, execution_id).await;
            return;
        }
    };

    // 2. Resolve templates
    let retry_policy = endpoint.get_retry_policy();

    let config_values = if let Some(ref config_name) = endpoint.config_ref {
        match load_config(ctx, db, config_name).await {
            Ok(vals) => vals,
            Err(e) => {
                tracing::error!(execution_id, "Config resolution failed: {}", e);
                let _ = db::executions::complete_failed(db, execution_id).await;
                record_attempt(
                    db,
                    execution_id,
                    attempt_count,
                    "FAILED",
                    started_at,
                    None,
                    Some(
                        &serde_json::json!({ "type": "TEMPLATE_RESOLUTION_FAILED", "message": e }),
                    ),
                )
                .await;
                log_execution(
                    db,
                    execution_id,
                    attempt_count,
                    "ERROR",
                    &format!("Template resolution failed: {}", e),
                )
                .await;
                return;
            }
        }
    } else {
        HashMap::new()
    };

    let secret_values = match invokr_common::secrets::load(
        db,
        &ctx.encryption_key,
        &endpoint.spec,
        Some(&ctx.secret_cache),
    )
    .await
    {
        Ok(vals) => vals,
        Err(e) => {
            tracing::error!(execution_id, "Secret resolution failed: {}", e);
            let _ = db::executions::complete_failed(db, execution_id).await;
            record_attempt(
                db,
                execution_id,
                attempt_count,
                "FAILED",
                started_at,
                None,
                Some(&serde_json::json!({ "type": "TEMPLATE_RESOLUTION_FAILED", "message": e })),
            )
            .await;
            log_execution(
                db,
                execution_id,
                attempt_count,
                "ERROR",
                &format!("Secret resolution failed: {}", e),
            )
            .await;
            return;
        }
    };

    let input_map: HashMap<String, serde_json::Value> = input
        .and_then(|v| v.as_object())
        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    let mut execution_map: HashMap<String, serde_json::Value> = HashMap::new();
    execution_map.insert("idempotency_key".into(), serde_json::json!(idempotency_key));
    execution_map.insert("attempt_count".into(), serde_json::json!(attempt_count));
    execution_map.insert("execution_id".into(), serde_json::json!(execution_id));
    execution_map.insert("job_id".into(), serde_json::json!(job_id));
    execution_map.insert("org_id".into(), serde_json::json!(org_id));
    execution_map.insert("workspace_id".into(), serde_json::json!(workspace_id));
    let cb_base = format!(
        "{base}{prefix}/v1/callbacks/{org}/{ws}/executions/{exec}",
        base = ctx.api_base_url.trim_end_matches('/'),
        prefix = ctx.path_prefix,
        org = org_id,
        ws = workspace_id,
        exec = execution_id,
    );
    execution_map.insert(
        "callback_url".into(),
        serde_json::json!(format!("{cb_base}/complete")),
    );
    execution_map.insert(
        "callback_url_success".into(),
        serde_json::json!(format!("{cb_base}/complete")),
    );
    execution_map.insert(
        "callback_url_failure".into(),
        serde_json::json!(format!("{cb_base}/fail")),
    );

    let resolved_spec = match template::resolve(
        &endpoint.spec,
        &input_map,
        &config_values,
        &secret_values,
        &execution_map,
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(execution_id, "Template resolution failed: {}", e);
            let _ = db::executions::complete_failed(db, execution_id).await;
            record_attempt(
                db,
                execution_id,
                attempt_count,
                "FAILED",
                started_at,
                None,
                Some(&serde_json::json!({ "type": "TEMPLATE_RESOLUTION_FAILED", "message": e })),
            )
            .await;
            log_execution(
                db,
                execution_id,
                attempt_count,
                "ERROR",
                &format!("Template resolution failed: {}", e),
            )
            .await;
            return;
        }
    };

    // 3. Inject job input as body if no body/body_template in resolved spec
    let mut dispatch_spec = resolved_spec;
    if dispatch_spec.get("body").is_none() && dispatch_spec.get("body_template").is_none() {
        if let Some(input_val) = input {
            if let Some(obj) = dispatch_spec.as_object_mut() {
                obj.insert("body".to_string(), input_val.clone());
            }
        }
    }

    // 4. Dispatch
    log_execution(
        db,
        execution_id,
        attempt_count,
        "INFO",
        &format!("Dispatching {} to {}", endpoint_type, endpoint_name),
    )
    .await;

    // Resolved once and reused below: the dispatcher needs the async status codes
    // to classify an "accepted, still working" response as a successful dispatch,
    // and the Success arm needs the rest of the config to set up polling.
    let async_cfg = endpoint.get_async_config();
    let async_status_codes: &[u16] = async_cfg
        .as_ref()
        .map(|c| c.status_codes.as_slice())
        .unwrap_or(&[]);

    let result = match endpoint_type {
        "HTTP" => {
            dispatcher::http::dispatch(
                &ctx.http_client,
                &dispatch_spec,
                idempotency_key,
                async_status_codes,
            )
            .await
        }
        "INTERNAL" => {
            dispatcher::internal::dispatch(&mut *db.conn, db.prefix, schema_name, &dispatch_spec)
                .await
        }
        #[cfg(feature = "kafka")]
        "KAFKA" => dispatcher::kafka::dispatch(&dispatch_spec).await,
        #[cfg(feature = "redis-stream")]
        "REDIS_STREAM" => dispatcher::redis_stream::dispatch(&dispatch_spec).await,
        _ => {
            tracing::error!(execution_id, "Unsupported endpoint type: {}", endpoint_type);
            DispatchResult::Failure {
                error: serde_json::json!({ "type": "UNSUPPORTED_TYPE", "message": format!("Unsupported: {}", endpoint_type) }),
            }
        }
    };

    let completed_at = Utc::now();
    let duration_ms = (completed_at - started_at).num_milliseconds();
    let duration_secs = duration_ms as f64 / 1000.0;

    // 5. Record attempt + finalize
    match result {
        DispatchResult::Success {
            output,
            headers,
            status_code,
        } => {
            if let Some(async_cfg) = &async_cfg {
                if async_cfg.status_codes.contains(&status_code) {
                    // Long-running mode: extract Location, transition to WAITING.
                    // Use per-job overrides (async_max_wait_ms / async_max_polls) when provided,
                    // falling back to the live endpoint spec defaults.
                    match headers.get("location") {
                        None => {
                            let err = serde_json::json!({"type": "MISSING_POLL_URL"});
                            let _ = db::executions::complete_failed(db, execution_id).await;
                            record_attempt(
                                db,
                                execution_id,
                                attempt_count,
                                "FAILED",
                                started_at,
                                None,
                                Some(&err),
                            )
                            .await;
                            log_execution(
                                db,
                                execution_id,
                                attempt_count,
                                "ERROR",
                                "Destination returned async status but no Location header",
                            )
                            .await;
                            return;
                        }
                        Some(loc) => {
                            let initial_url = dispatch_spec["url"].as_str().unwrap_or_default();
                            let poll_url = crate::poll::resolve_relative_url(initial_url, loc);
                            let now = chrono::Utc::now();
                            // Resolve effective limits: per-job snapshot wins over endpoint default.
                            let effective_max_wait_ms =
                                async_max_wait_ms.unwrap_or(async_cfg.max_wait_ms);
                            let effective_max_polls =
                                async_max_polls.unwrap_or(async_cfg.max_polls);
                            let deadline =
                                now + chrono::Duration::milliseconds(effective_max_wait_ms);
                            // For callback-only endpoints (no poll config), skip wakeup until
                            // deadline to avoid claiming every second and finding nothing to do.
                            let next_run_at = if async_cfg.poll.as_ref().is_none() {
                                deadline
                            } else {
                                let initial_delay = crate::poll::parse_retry_after(
                                    headers.get("retry-after").map(|s| s.as_str()),
                                )
                                .unwrap_or_else(|| {
                                    async_cfg
                                        .poll
                                        .as_ref()
                                        .map(|p| p.initial_delay_ms)
                                        .unwrap_or(1000)
                                });
                                std::cmp::min(
                                    deadline,
                                    now + chrono::Duration::milliseconds(initial_delay),
                                )
                            };
                            let _ = db::executions::transition_to_waiting(
                                db,
                                execution_id,
                                &poll_url,
                                now,
                                deadline,
                                next_run_at,
                                effective_max_wait_ms,
                                effective_max_polls,
                            )
                            .await;
                            metrics::gauge!(invokr_common::metrics::EXECUTIONS_WAITING,
                                "schema" => schema_name.to_string(),
                            )
                            .increment(1.0);
                            let initial_delay_for_log = (next_run_at - now).num_milliseconds();
                            record_attempt(
                                db,
                                execution_id,
                                attempt_count,
                                "WAITING",
                                started_at,
                                Some(&output),
                                None,
                            )
                            .await;
                            log_execution(db, execution_id, attempt_count, "INFO",
                                &format!("Entered WAITING; will poll {poll_url} in {initial_delay_for_log}ms")).await;
                            return;
                        }
                    }
                }
            }

            metrics::counter!(m::EXECUTIONS_COMPLETED_TOTAL,
                "status" => "SUCCESS",
                "schema" => schema_name.to_string(),
                "endpoint" => endpoint_name.to_string(),
            )
            .increment(1);
            metrics::histogram!(m::EXECUTION_DURATION_SECONDS,
                "status" => "SUCCESS",
                "endpoint" => endpoint_name.to_string(),
                "endpoint_type" => endpoint_type.to_string(),
            )
            .record(duration_secs);

            record_attempt(
                db,
                execution_id,
                attempt_count,
                "SUCCESS",
                started_at,
                Some(&output),
                None,
            )
            .await;
            let _ = db::executions::complete_success(db, execution_id, &output).await;
            log_execution(
                db,
                execution_id,
                attempt_count,
                "INFO",
                &format!("Execution succeeded in {}ms", duration_ms),
            )
            .await;
        }
        DispatchResult::Failure { error } => {
            record_attempt(
                db,
                execution_id,
                attempt_count,
                "FAILED",
                started_at,
                None,
                Some(&error),
            )
            .await;

            if attempt_count < max_attempts {
                let backoff_ms = backoff::compute_backoff(&retry_policy, attempt_count);
                let _ = db::executions::complete_retry(db, execution_id, backoff_ms).await;
                log_execution(
                    db,
                    execution_id,
                    attempt_count,
                    "WARN",
                    &format!(
                        "Attempt {} failed, retrying in {}ms: {}",
                        attempt_count, backoff_ms, error
                    ),
                )
                .await;
            } else {
                metrics::counter!(m::EXECUTIONS_COMPLETED_TOTAL,
                    "status" => "FAILED",
                    "schema" => schema_name.to_string(),
                    "endpoint" => endpoint_name.to_string(),
                )
                .increment(1);
                metrics::histogram!(m::EXECUTION_DURATION_SECONDS,
                    "status" => "FAILED",
                    "endpoint" => endpoint_name.to_string(),
                    "endpoint_type" => endpoint_type.to_string(),
                )
                .record(duration_secs);

                let _ = db::executions::complete_failed(db, execution_id).await;
                log_execution(
                    db,
                    execution_id,
                    attempt_count,
                    "ERROR",
                    &format!(
                        "Execution failed after {} attempts: {}",
                        attempt_count, error
                    ),
                )
                .await;
            }
        }
    }
}

async fn load_config(
    ctx: &PipelineContext,
    db: &mut DbContext<'_>,
    name: &str,
) -> Result<HashMap<String, serde_json::Value>, String> {
    if let Some(cached) = ctx.config_cache.get(name) {
        return flatten_json_object(&cached);
    }

    let config = db::configs::get(db, name)
        .await
        .map_err(|e| format!("Failed to load config '{}': {}", name, e))?
        .ok_or_else(|| format!("Config '{}' not found", name))?;

    ctx.config_cache
        .set(name.to_string(), config.values_json.clone());
    flatten_json_object(&config.values_json)
}

fn flatten_json_object(
    value: &serde_json::Value,
) -> Result<HashMap<String, serde_json::Value>, String> {
    let obj = value
        .as_object()
        .ok_or("Config values must be a JSON object")?;
    Ok(obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

async fn record_attempt(
    db: &mut DbContext<'_>,
    execution_id: &str,
    attempt_number: i64,
    status: &str,
    started_at: chrono::DateTime<Utc>,
    output: Option<&serde_json::Value>,
    error: Option<&serde_json::Value>,
) {
    let completed_at = Utc::now();
    let duration_ms = (completed_at - started_at).num_milliseconds();
    if let Err(e) = db::attempts::insert(
        db,
        execution_id,
        attempt_number,
        status,
        started_at,
        completed_at,
        duration_ms,
        output,
        error,
    )
    .await
    {
        tracing::error!(execution_id, "Failed to record attempt: {}", e);
    }
}

async fn log_execution(
    db: &mut DbContext<'_>,
    execution_id: &str,
    attempt_number: i64,
    level: &str,
    message: &str,
) {
    if let Err(e) =
        db::execution_logs::insert(db, execution_id, attempt_number, level, message).await
    {
        tracing::error!(execution_id, "Failed to write execution log: {}", e);
    }
}
