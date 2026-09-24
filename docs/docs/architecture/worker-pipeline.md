---
id: worker-pipeline
title: Worker Pipeline
---

# Worker Pipeline

The worker pipeline is the core execution engine of Invokr. It polls the database for actionable executions, claims them via `SELECT FOR UPDATE SKIP LOCKED`, resolves templates, dispatches to the target endpoint, and records the outcome, all within a scoped transaction.

## The poller

The poller is the main loop of the worker process. It runs as a single tokio task that spawns concurrent execution tasks, gated by a semaphore.

### Semaphore-gated concurrency

The poller uses a `tokio::sync::Semaphore` to limit the number of concurrent in-flight executions. The default is 50 concurrent jobs, configurable via `INVOKR_WORKER_MAX_CONCURRENT`.

```rust
let semaphore = Arc::new(Semaphore::new(config.worker.max_concurrent));
```

Each poll iteration:

1. Acquires a semaphore permit (blocks if all permits are in use — natural backpressure)
2. Fetches the list of active workspace schemas via `SchemaProvider::get_active_schemas()`
3. Spawns a tokio task that attempts to claim and process one execution
4. The spawned task releases the permit on completion, unblocking the next iteration

### Iterating active schemas

The poller iterates all active workspace schemas returned by the `SchemaRegistry` (cached, 30s TTL). For each schema, it begins a scoped transaction (`scoped_transaction`) that sets `search_path` to the workspace schema, then attempts to claim an execution:

```rust
for schema_name in schemas {
    let mut tx = db::scoped::scoped_transaction(pool, schema_name).await?;
    let mut db = DbContext::new(&mut *tx, prefix);

    let exec = match db::executions::claim(&mut db, worker_id).await {
        Ok(Some(exec)) => exec,
        Ok(None) => continue,
        Err(e) => { /* log and continue */ }
    };

    pipeline::process_execution(&ctx, &mut db, schema_name, &exec, ...).await;
    tx.commit().await?;
    return true; // found work
}
```

The claim query uses `SELECT FOR UPDATE SKIP LOCKED` within the transaction, ensuring that no two workers can claim the same execution:

```sql
UPDATE executions
SET status = 'RUNNING',
    worker_id = $1,
    started_at = now(),
    attempt_count = attempt_count + 1
WHERE execution_id = (
    SELECT execution_id
    FROM executions
    WHERE status IN ('QUEUED', 'RETRYING', 'PENDING')
      AND run_at <= now()
    ORDER BY run_at ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
RETURNING execution_id, job_id, endpoint, endpoint_type, input, attempt_count, max_attempts;
```

### Idle backoff

When no work is found across all schemas, the poller enters idle mode. An `AtomicBool` flag tracks whether the previous iteration found work. If idle, the poller sleeps for the configured poll interval (default 200ms via `INVOKR_WORKER_POLL_INTERVAL_MS`) before trying again:

```rust
if idle.load(Ordering::Relaxed) {
    tokio::select! {
        _ = tokio::time::sleep(poll_interval) => {
            idle.store(false, Ordering::Relaxed);
        }
        _ = cancel.cancelled() => { break; }
    }
}
```

While work is available, the poller spins freely (no sleep), only blocking on semaphore permit availability, which keeps latency sub-second for immediate jobs.

### Graceful shutdown

On shutdown (triggered by `CancellationToken`), the poller:

1. Stops accepting new work (breaks out of the poll loop)
2. Waits for all in-flight tasks to complete, up to a configurable timeout (default 30s via `INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC`)
3. Acquires all permits from the semaphore to ensure all spawned tasks have finished

```rust
let _ = tokio::time::timeout(timeout, async {
    let _all = semaphore
        .acquire_many(config.worker.max_concurrent as u32)
        .await;
}).await;
```

:::note
Any execution still in `RUNNING` state after the timeout expires will remain in that state. A stuck execution reclaimer (or manual intervention) would be needed to reset it to `RETRYING` or `FAILED`.
:::

## PipelineContext

The `PipelineContext` is shared across all execution tasks. It holds all the resources needed for template resolution and dispatch:

```rust
pub struct PipelineContext {
    pub pool: PgPool,
    pub http_client: Client,
    pub config_cache: ConfigCache,
    pub secret_cache: SecretCache,
    pub encryption_key: String,
    pub table_prefix: String,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `pool` | `PgPool` | Database connection pool (shared across all tasks) |
| `http_client` | `reqwest::Client` | HTTP client with keep-alive connection pooling |
| `config_cache` | `ConfigCache` | In-memory cache of config values (60s TTL) |
| `secret_cache` | `SecretCache` | In-memory cache of decrypted secrets (300s TTL) |
| `encryption_key` | `String` | AES encryption key for decrypting secrets at rest |
| `table_prefix` | `String` | Table name prefix (e.g. `sched_` or empty string) |

The context is wrapped in `Arc` and cloned for each spawned task, so all caches and clients are shared rather than duplicated.

## process_execution() steps

The `process_execution` function is the core pipeline. It receives the claimed execution and runs through the following steps:

### 1. Load endpoint

Loads the endpoint definition from the database (within the scoped transaction):

```rust
let endpoint = match db::endpoints::get(db, endpoint_name).await {
    Ok(Some(ep)) => ep,
    Ok(None) => {
        // Mark execution as FAILED — endpoint was deleted
        let _ = db::executions::complete_failed(db, execution_id).await;
        return;
    }
    Err(e) => { /* ... */ }
};
```

### 2. Load config (cached)

If the endpoint references a config, it's loaded from the `ConfigCache` first. On cache miss, it's fetched from the DB and cached:

```rust
let config_values = if let Some(ref config_name) = endpoint.config_ref {
    match load_config(ctx, db, config_name).await {
        Ok(vals) => vals,
        Err(e) => {
            // Config resolution failed — fail immediately, no retry
            let _ = db::executions::complete_failed(db, execution_id).await;
            return;
        }
    }
} else {
    HashMap::new()
};
```

:::warning
Template resolution failures (missing config, missing secret, unresolvable template variable) cause the execution to fail **immediately** without retries. Since the same failure would recur on every retry, retrying is wasteful.
:::

### 3. Load secrets (cached, decrypt)

Secrets referenced in the endpoint spec are extracted by scanning for `{{secret.*}}` patterns, then loaded from the `SecretCache`. On cache miss, they're fetched from the DB (encrypted at rest) and decrypted using the AES encryption key:

```rust
let decrypted = crypto::decrypt(&secret.encrypted_value, &ctx.encryption_key)?;
ctx.secret_cache.set(name.to_string(), decrypted.clone());
```

### 4. Resolve templates

All template variables in the endpoint spec are resolved from three namespaces:

| Namespace | Source | Description |
|-----------|--------|-------------|
| `{{input.*}}` | Job input payload | Per-execution, from the job creation request |
| `{{config.*}}` | Config values (cached) | Static variables like base URLs |
| `{{secret.*}}` | Secret store (cached, decrypted) | API keys, credentials |
| `{{execution.*}}` | Execution metadata | `idempotency_key`, `attempt_count`, `execution_id`, `job_id` |

```rust
let resolved_spec = template::resolve(
    &endpoint.spec,
    &input_map,
    &config_values,
    &secret_values,
    &execution_map,
)?;
```

### 5. Inject body

If the resolved spec has no `body` or `body_template` field, the job's `input` is injected directly as the HTTP request body:

```rust
if dispatch_spec.get("body").is_none() && dispatch_spec.get("body_template").is_none() {
    if let Some(input_val) = input {
        if let Some(obj) = dispatch_spec.as_object_mut() {
            obj.insert("body".to_string(), input_val.clone());
        }
    }
}
```

### 6. Dispatch

The resolved spec is dispatched to the appropriate transport based on `endpoint_type`:

```rust
let result = match endpoint_type {
    "HTTP" => dispatcher::http::dispatch(&ctx.http_client, &dispatch_spec, idempotency_key).await,
    "INTERNAL" => dispatcher::internal::dispatch(&mut *db.conn, db.prefix, schema_name, &dispatch_spec).await,
    #[cfg(feature = "kafka")]
    "KAFKA" => dispatcher::kafka::dispatch(&dispatch_spec).await,
    #[cfg(feature = "redis-stream")]
    "REDIS_STREAM" => dispatcher::redis_stream::dispatch(&dispatch_spec).await,
    _ => DispatchResult::Failure { error: /* UNSUPPORTED_TYPE */ },
};
```

The dispatch returns a `DispatchResult`:

```rust
pub enum DispatchResult {
    Success { output: serde_json::Value },
    Failure { error: serde_json::Value },
}
```

### 7. Record attempt

An attempt row is inserted recording the attempt number, status, start time, completion time, duration, output (on success), or error (on failure):

```rust
db::attempts::insert(
    db, execution_id, attempt_number, status,
    started_at, completed_at, duration_ms,
    output, error,
).await;
```

### 8. Finalize

The execution's final state depends on the dispatch result:

**On success:**

```rust
db::executions::complete_success(db, execution_id, &output).await;
// Execution status → SUCCESS
```

**On failure with retries remaining** (`attempt_count < max_attempts`):

```rust
let backoff_ms = backoff::compute_backoff(&retry_policy, attempt_count);
db::executions::complete_retry(db, execution_id, backoff_ms).await;
// Execution status → RETRYING, run_at = now() + backoff
```

**On failure with retries exhausted** (`attempt_count >= max_attempts`):

```rust
db::executions::complete_failed(db, execution_id).await;
// Execution status → FAILED
```

All of this happens within the scoped transaction — the execution state change, attempt record, and execution logs commit atomically. See [Exactly-Once Guarantees](./exactly-once) for details.

## Execution logs

Throughout the pipeline, structured execution logs are written to the `execution_logs` table. These logs are visible in the dashboard and via the `GET /v1/executions/{id}/logs` API:

```rust
async fn log_execution(db, execution_id, attempt_number, level, message).await;
```

Log levels used:

| Level | When |
|-------|------|
| `INFO` | Dispatch started, execution succeeded |
| `WARN` | Attempt failed, retrying |
| `ERROR` | Endpoint not found, template resolution failed, execution failed after all attempts |

## Metrics

The pipeline emits Prometheus metrics at each stage:

| Metric | Type | Description |
|--------|------|-------------|
| `invokr_executions_claimed_total` | Counter | Executions claimed by workers, by schema and endpoint type |
| `invokr_executions_completed_total` | Counter | Executions completed, by status (SUCCESS/FAILED) |
| `invokr_execution_duration_seconds` | Histogram | End-to-end execution duration |
| `invokr_dispatch_total` | Counter | Dispatch attempts by endpoint type |
| `invokr_dispatch_duration_seconds` | Histogram | Dispatcher-level latency |
| `invokr_worker_inflight_executions` | Gauge | Currently in-flight executions per worker |
| `invokr_worker_poll_idle_total` | Counter | Idle poll cycles (no work found) |

See [Architecture Overview](./overview) for the full metrics reference.
