---
id: library-mode
title: Library Mode Setup
---

# Library Mode Setup

Library mode (also called **embedded mode**) embeds Invokr directly into your Rust application process. Your code holds a `PgPool` and accesses the database directly, with no HTTP overhead and no separate API server. A background worker task runs inside your process via `start_worker()`.

This is the fastest way to add durable job scheduling to a single Rust application. For the conceptual model and a comparison with service mode, see [Dual Deployment Modes](../architecture/dual-deployment).

---

## Prerequisites

- **PostgreSQL 16+** with the `pg_cron` extension. See [Docker](./docker) for the custom PostgreSQL image.
- **Base migrations applied** to the `public` schema. The four migration files in `migrations/` create the shared `organizations` and `workspaces` tables plus the `pg_cron` setup:
  ```bash
  for f in migrations/20260317000000_initial.sql \
           migrations/20260318000000_multi_tenancy.sql \
           migrations/20260322000000_txn_based_pickup.sql \
           migrations/20260322000001_pg_cron.sql; do
    psql -U invokr -d invokr_db -v ON_ERROR_STOP=1 < "$f"
  done
  ```
- **Rust toolchain** (stable). The workspace MSRV is 1.75.

:::tip
For local development, `just setup` starts PostgreSQL and applies all migrations automatically.
:::

---

## Add the dependency

`invokr-worker` is not published to crates.io. Add it as a git dependency:

```toml
[dependencies]
invokr-worker = { git = "https://github.com/juspay/invokr", branch = "main" }
```

### Feature flags

| Feature | Description |
|---------|-------------|
| `kafka` | Kafka dispatcher support via `rdkafka` |
| `redis-stream` | Redis Stream dispatcher support via `redis` |
| `kms` | AWS KMS integration for at-rest secret encryption |

```toml
# Example: Kafka + Redis Stream dispatchers
invokr-worker = { git = "https://github.com/juspay/invokr", branch = "main", features = ["kafka", "redis-stream"] }

# Example: KMS-encrypted secrets at rest
invokr-worker = { git = "https://github.com/juspay/invokr", branch = "main", features = ["kms"] }
```

Without a feature, the corresponding endpoint type returns an `UNSUPPORTED_TYPE` error at dispatch time.

---

## Generate an encryption key

Invokr encrypts secret values at rest using AES-256-GCM. The key is a 64-character hex string (32 bytes). Generate one with:

```bash
openssl rand -hex 32
# example output: a1b2c3d4e5f6...
```

:::danger
**In production, always use a strong, randomly generated key.** The default all-zeros key (`0000...0000`) provides no security. If the key is rotated, existing secrets encrypted with the old key cannot be decrypted.
:::

For local development without secrets, passing 64 zeros is acceptable. See [Environment Variables](../configuration/environment-variables) (`INVOKR_ENCRYPTION_KEY`) and [AWS KMS Integration](./kms) for production key management.

---

## Construct the client

`InvokrLibraryClient::new` takes a caller-owned `PgPool` and accesses the database directly:

```rust
use invokr_worker::{InvokrClient, InvokrLibraryClient};

let pool = sqlx::PgPool::connect(&database_url).await?;
let client = InvokrLibraryClient::new(
    pool,                    // caller-owned PgPool
    "",                      // table prefix ("" = no prefix; "sched_" = sched_jobs)
    "64_hex_chars...",       // AES-256 encryption key
    None,                    // optional reqwest client to reuse
)?;
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `pool` | `PgPool` | Caller-owned connection pool pointing at the same PostgreSQL instance |
| `table_prefix` | `&str` | Prefix for all Invokr tables. Pass the full prefix including trailing underscore (e.g. `"sched_"` → `sched_jobs`). `""` means no prefix. Only alphanumeric and underscore allowed. |
| `encryption_key` | `&str` | 64 hex-char AES-256 key for secrets; pass zeros if not using secrets |
| `http_client` | `Option<Client>` | Optional reqwest client to reuse the caller's connection pool |

:::note
For convenience, `InvokrLibraryClient::from_database_url(database_url, max_connections, ...)` builds an internal pool. Use `client.pool()` to access it for a `SchemaProvider`.
:::

---

## Provision a workspace

Each workspace gets its own PostgreSQL schema with isolated tables. Call `provision_workspace()` to create the schema and apply the workspace DDL template:

```rust
client.provision_workspace("my_schema").await?;
```

This is idempotent (`CREATE SCHEMA IF NOT EXISTS`, `CREATE TABLE IF NOT EXISTS`). The `{p}` placeholder in `workspace_v1.sql` is replaced by the `table_prefix` verbatim — so with prefix `"sched_"` you get `sched_jobs`, `sched_executions`, etc.

:::info
In library mode, `provision_workspace()` only creates the tenant schema and tables. It does **not** insert into `public.organizations` or `public.workspaces` — those are managed by the caller (or skipped entirely if you use a custom `SchemaProvider`).
:::

---

## Register an endpoint and fire a job

Register an HTTP endpoint (upsert — safe to call on every startup):

```rust
use invokr_worker::JobTrigger;

client.register_endpoint(
    "my_schema",
    "send-email",
    "HTTP",
    serde_json::json!({
        "url": "https://api.example.com/emails",
        "method": "POST",
        "headers": { "Content-Type": "application/json" },
        "body_template": { "user_id": "{{input.user_id}}" },
        "timeout_ms": 5000,
        "expected_status_codes": [200, 201, 202]
    }),
    None, // retry_policy: None uses defaults
).await?;
```

Fire an immediate job:

```rust
let execution_id = client.create_job(
    "my_schema",
    "send-email",
    serde_json::json!({ "user_id": "u_123" }),
    3,                          // max_attempts
    JobTrigger::Immediate,
    Some("user-123-email-1"),   // idempotency_key
).await?;
```

See [HTTP Endpoints](../guides/http-endpoints) for template variables, [Payload Specs](../core-concepts/payload-specs) for input validation, and [Retry Policy](../core-concepts/retry-policy) for backoff configuration.

---

## Start the worker

The worker runs as a background tokio task. `start_worker()` takes a `SchemaProvider` (which tells the worker which workspace schemas to poll) and a `WorkerConfig`:

```rust
use invokr_worker::WorkerConfig;
use invokr_common::tenant::SchemaProvider;
use std::future::Future;

// A minimal SchemaProvider that returns a fixed list of schemas.
// Use this when your host app maintains its own list of workspaces.
struct MySchemaProvider;

impl SchemaProvider for MySchemaProvider {
    fn get_active_schemas(&self) -> impl Future<Output = Result<Vec<String>, sqlx::Error>> + Send {
        async { Ok(vec!["my_schema".to_string()]) }
    }
}

let handle = client.start_worker(MySchemaProvider, WorkerConfig::default());
```

### SchemaProvider

The `SchemaProvider` trait tells the worker where to find the list of active workspace schemas:

```rust
pub trait SchemaProvider: Send + Sync + 'static {
    fn get_active_schemas(&self) -> impl Future<Output = Result<Vec<String>, sqlx::Error>> + Send;
}
```

| Implementation | When to use |
|---------------|-------------|
| **Custom** (shown above) | Library mode — your host app knows its own schemas |
| `SchemaRegistry::new(pool, 30)` | Service mode — queries `public.workspaces` with a 30s TTL cache |

In library mode, `provision_workspace()` does not insert into `public.workspaces`, so `SchemaRegistry` won't find your workspace. Use a custom `SchemaProvider` that returns schemas from your own configuration.

### WorkerConfig

```rust
pub struct WorkerConfig {
    pub max_concurrent: usize,      // default: 50
    pub poll_interval_ms: u64,       // default: 200
    pub config_cache_ttl_sec: u64,  // default: 60
    pub secret_cache_ttl_sec: u64,  // default: 300
    pub shutdown_timeout_sec: u64,  // default: 30
}
```

### WorkerHandle

`start_worker()` returns a `WorkerHandle` that owns the cancellation token and task handle:

```rust
pub struct WorkerHandle { /* ... */ }

impl WorkerHandle {
    /// Signal the worker to stop. Returns immediately.
    pub fn shutdown(&self);
    /// Wait for the worker task to finish.
    pub async fn join(self) -> anyhow::Result<()>;
}
```

---

## Host-app lifecycle integration

Wire the worker's shutdown into your application's shutdown signal. The `WorkerHandle` owns its own `CancellationToken` — you don't need `tokio-util` as a direct dependency:

```rust
let handle = client.start_worker(schema_provider, WorkerConfig::default());

// Main application loop...
tokio::select! {
    _ = tokio::signal::ctrl_c() => {
        tracing::info!("received Ctrl+C, shutting down");
    }
    _ = app_shutdown_signal() => {
        tracing::info!("app shutdown signal received");
    }
}

// Graceful drain: stop polling, let in-flight jobs finish (bounded by shutdown_timeout_sec).
handle.shutdown();
handle.join().await?;
```

During shutdown:
1. The worker stops polling for new executions
2. In-flight executions are allowed to complete
3. If `shutdown_timeout_sec` is reached, remaining executions are abandoned (they will be reclaimed by other workers via the stuck execution reclaimer)

:::warning
Set `shutdown_timeout_sec` high enough for your longest-running job. If a job takes 45 seconds and the timeout is 30 seconds, the job will be interrupted and retried.
:::

---

## Complete runnable example

A full runnable example lives at [`examples/library-mode/`](https://github.com/juspay/invokr/tree/main/examples/library-mode) in the repository. It provisions a workspace, registers an HTTP endpoint pointing at the mock server, fires an immediate job, starts the worker, waits for the execution to complete, and shuts down gracefully.

### Prerequisites

```bash
# Terminal 1: start PostgreSQL + run migrations
just setup

# Terminal 2: start the mock HTTP server (port 9999)
just mock-server
```

### Run the example

```bash
just example-library-mode
# or: cargo run -p library-mode-example
```

### Key parts of the example

The example uses a custom `SchemaProvider` since `provision_workspace()` in library mode doesn't insert into `public.workspaces`:

```rust
struct StaticSchemaProvider;

impl SchemaProvider for StaticSchemaProvider {
    fn get_active_schemas(&self) -> impl Future<Output = Result<Vec<String>, sqlx::Error>> + Send {
        async { Ok(vec!["library_example".to_string()]) }
    }
}
```

The full flow: connect pool → construct client → provision workspace → register endpoint → fire job → start worker → wait for completion or Ctrl+C → shutdown → join:

```rust
let pool = sqlx::PgPool::connect(&database_url).await?;
let client = InvokrLibraryClient::new(pool, "", ENCRYPTION_KEY, None)?;

client.provision_workspace("library_example").await?;
client.register_endpoint("library_example", "ping", "HTTP", /* ... */, None).await?;

let execution_id = client.create_job(
    "library_example", "ping", json!({}), 3, JobTrigger::Immediate, None,
).await?;

let handle = client.start_worker(StaticSchemaProvider, WorkerConfig::default());

tokio::select! {
    _ = tokio::signal::ctrl_c() => { /* ... */ }
    _ = wait_for_completion(&client, &execution_id) => { /* ... */ }
}

handle.shutdown();
handle.join().await?;
```

See [`examples/library-mode/src/main.rs`](https://github.com/juspay/invokr/blob/main/examples/library-mode/src/main.rs) for the complete source.

---

## Switching to service mode

If you later need to run Invokr as a standalone service (e.g. multiple apps sharing one Invokr deployment), swap `InvokrLibraryClient` for `InvokrHttpClient` at the construction site:

```rust
use invokr_worker::InvokrHttpClient;

// Before (library mode):
// let client = InvokrLibraryClient::new(pool, "", &key, None)?;

// After (service mode):
let client = InvokrHttpClient::new(
    "http://localhost:8080".to_string(),  // base URL
    "your-api-key".to_string(),           // API key
    "org_id".to_string(),                 // org ID
);
```

Call sites that use the `InvokrClient` trait are unchanged — the trait abstracts over both modes. This is a code change at the construction site only, not a per-call-site change.

See [Dual Deployment Modes](../architecture/dual-deployment) for the full comparison and [Docker](./docker) / [Production Deployment](./production) for service-mode setup.

---

## See also

- [Dual Deployment Modes](../architecture/dual-deployment) — conceptual model and method-by-method comparison
- [Docker](./docker) — PostgreSQL image with `pg_cron`, dev compose stack
- [Environment Variables](../configuration/environment-variables) — full configuration reference
- [AWS KMS Integration](./kms) — encrypting the encryption key at rest
- [HTTP Endpoints](../guides/http-endpoints) — endpoint spec and template resolution
- [Core Concepts](../core-concepts/overview) — the three-step workflow
