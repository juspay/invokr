---
id: overview
title: Architecture Overview
---

# Architecture Overview

Invokr is a distributed job scheduling and execution engine built in Rust. It provides durable, de-duplicated, retriable delivery of jobs to HTTP endpoints, Kafka topics, and Redis Streams — with type-safety guarantees. Scheduling is exactly-once; delivery is at-least-once (see [Delivery Guarantees](./exactly-once)).

## System Architecture

```
                              ┌─────────────────────────┐
                              │        Client / SDK      │
                              └────────────┬────────────┘
                                           │
                                    POST /v1/jobs
                                           │
                              ┌────────────▼────────────┐
                              │   API Server (actix-web) │
                              │   port 8080 + /metrics   │
                              └────────────┬────────────┘
                                           │
                             INSERT job + execution (txn)
                                           │
                   ┌─────────────────────────▼──────────────────────────┐
                   │               PostgreSQL + pg_cron                  │
                   │                                                     │
                   │  Source of truth          CRON scheduling natively  │
                   │  FOR UPDATE SKIP LOCKED   via pg_cron extension    │
                   │  Txn-based job pickup     (no external scheduler)  │
                   └───────┬──────────────────────────────┬─────────────┘
                           │                              │
                ┌──────────▼───────────┐    ┌─────────────▼─────────────┐
                │     Worker Pool      │    │    Dashboard (WASM)       │
                │                      │    │    Leptos + Trunk         │
                │  Semaphore-gated     │    │    port 3000              │
                │  50 concurrent jobs  │    └───────────────────────────┘
                │                      │
                │  ┌────────────────┐  │
                │  │ HTTP  (reqwest)│  │
                │  │ Kafka (rdkafka)│  │
                │  │ Redis (redis)  │  │
                │  └────────────────┘  │
                │  metrics on :9090    │
                └──────────────────────┘
```

## Process Topology

Invokr consists of four primary components:

| Component | Technology | Port | Description |
|-----------|-----------|------|-------------|
| **API Server** | actix-web (Rust) | 8080 | REST API for all CRUD operations, job invocation, Prometheus metrics at `/metrics` |
| **Worker Pool** | tokio (Rust) | 9090 (metrics) | Execution engine that polls the DB for pending work, resolves templates, and dispatches to endpoints |
| **PostgreSQL + pg_cron** | PostgreSQL | 5432 | Source of truth for all state; pg_cron extension handles CRON scheduling natively |
| **Dashboard** | Leptos/WASM | 3000 | Web UI showing jobs, executions, attempts, and execution logs |

### API Server

The API server is built with [actix-web](https://actix.rs/). It handles all REST endpoints for organizations, workspaces, payload specs, configs, secrets, endpoints, jobs, and executions. On job creation, the API inserts both the job and its initial execution in a single database transaction — ensuring atomicity. The server also exposes Prometheus metrics at `GET /metrics`.

### Worker Pool

The worker is a tokio-based async process. It uses a semaphore to limit concurrency (default 50 concurrent jobs). Each poll iteration claims an execution via `SELECT FOR UPDATE SKIP LOCKED`, spawns a tokio task for the execution pipeline, and releases the permit on completion. The worker supports three dispatch types: HTTP (via reqwest), Kafka (via rdkafka), and Redis Streams (via redis-rs).

### PostgreSQL + pg_cron

PostgreSQL is the single source of truth for all state — jobs, executions, attempts, execution logs, configs, secrets, and endpoints. The `pg_cron` extension handles CRON job materialization natively: when a CRON job is created, it's registered with `cron.schedule()`. Each CRON tick inserts a new `QUEUED` execution directly into the database. No separate scheduler process is needed.

### Dashboard

The dashboard is a single-page application built with [Leptos](https://leptos.rs/) compiled to WebAssembly. It provides a visual interface for monitoring jobs, executions, attempts, and execution logs. The dashboard is excluded from the workspace build and compiled separately via Trunk.

## Crate Dependency Graph

```
                    ┌─────────────────┐
                    │  invokr-common   │
                    │  (models, DB,    │
                    │   config, tenant,│
                    │   cache, metrics)│
                    └────┬────────┬────┘
                         │        │
              ┌──────────▼──┐  ┌──▼──────────┐
              │  invokr-api  │  │ invokr-worker │
              │  (actix-web) │  │ (tokio)      │
              └──────────────┘  └───────────────┘

              ┌─────────────────┐
              │ invokr-dashboard │  (standalone, excluded from workspace)
              │  (Leptos/WASM)   │
              └─────────────────┘
```

| Crate | Description |
|-------|-------------|
| `invokr-common` | Shared library — models, DB layer, config, tenant management, caching, metrics, template resolution, crypto, pagination |
| `invokr-api` | REST API server (actix-web). CRUD for all resources, job invocation, Prometheus metrics at `/metrics` |
| `invokr-worker` | Execution engine. Polls DB for QUEUED/RETRYING/PENDING executions, resolves templates, dispatches to endpoints. Exposes metrics via HTTP listener |
| `invokr-mock-server` | Test fixture — HTTP server on port 9999 for integration tests |
| `invokr-dashboard` | Web UI — Leptos/WASM, shows jobs, executions, attempts. Excluded from workspace build |
| `invokr-sdk` | Generated Rust SDK from Smithy models. Excluded from workspace build (different MSRV 1.82) |

Both `invokr-api` and `invokr-worker` depend on `invokr-common`. The dashboard and SDK are standalone — they communicate with Invokr exclusively through the REST API.

## Data Flow

The end-to-end flow for a job is:

```
Client → POST /v1/jobs → API Server → INSERT job + execution (transaction) → PostgreSQL
                                                                              ↓
Worker polls DB (SKIP LOCKED) ← PostgreSQL ← pg_cron tick (CRON jobs only)
                                                                              ↓
Worker claims execution → Load endpoint → Load config (cached) → Load secrets (cached, decrypt)
                                                                              ↓
Resolve templates → Inject body → Dispatch (HTTP/Kafka/Redis) → Record attempt
                                                                              ↓
Finalize: SUCCESS / RETRYING (backoff) / FAILED → Commit transaction
```

### Immediate Job Flow

1. Client sends `POST /v1/jobs { trigger: IMMEDIATE }`
2. API inserts job + execution (`QUEUED`) in a single transaction, returns `201`
3. Worker poller (~200ms) claims execution via `SKIP LOCKED`, spawns task
4. Pipeline: cache-hit config → resolve templates → dispatch → record attempt
5. Finalize: mark execution `SUCCESS`, commit transaction
6. Total latency: ~300ms

### Delayed Job Flow

1. Client sends `POST /v1/jobs { trigger: DELAYED, run_at: "..." }`
2. API inserts job + execution (`PENDING` with `run_at`) in a transaction
3. Worker poller picks it up when `run_at <= now()` — no promoter needed
4. Pipeline executes as normal
5. Fires within ~200ms of `run_at` (worker poll interval)

### CRON Job Flow

1. Client sends `POST /v1/jobs { trigger: CRON, cron: "..." }`
2. API inserts job (`ACTIVE`, `cron_next_run_at` set) and registers with `cron.schedule()`
3. On each CRON tick, pg_cron inserts a new `QUEUED` execution with idempotency key `cron_{job_id}_{epoch_ms}`
4. Worker picks up the execution via normal `SKIP LOCKED` path
5. Repeats until the job is cancelled or its `cron_ends_at` window expires

## How Scheduling Works

Invokr uses **PostgreSQL pg_cron** for CRON materialization and **transaction-based pickup** for all job types. There is no separate scheduler process:

- **IMMEDIATE** jobs: Execution created as `QUEUED` in the same transaction as the job. Workers pick it up directly.
- **DELAYED** jobs: Execution created as `PENDING` with a `run_at` timestamp. Workers pick up PENDING executions once `run_at <= now()` — no promoter loop needed.
- **CRON** jobs: Registered with pg_cron at creation time. pg_cron inserts a new `QUEUED` execution on each tick. Workers pick it up directly.

The worker's claim query covers all three statuses in a single index:

```sql
WHERE status IN ('QUEUED', 'RETRYING', 'PENDING') AND run_at <= now()
```

See [Database-Driven Scheduling](./db-driven-scheduling) for details.

## Multi-Tenancy Architecture

Invokr uses **schema-per-tenant** isolation. Each workspace gets its own PostgreSQL schema with isolated tables. Shared tables live in the `public` schema:

```
public schema:        organizations, workspaces
tenant schema:        payload_specs, configs, secrets, endpoints,
(org_workspace):      jobs, executions, attempts, execution_logs
```

Tenant-scoped API requests require `X-Org-Id` and `X-Workspace-Id` headers. The worker iterates all active workspace schemas via a cached `SchemaRegistry` (30s TTL) that queries `public.workspaces` for active schemas.

The schema name is derived from the org ID and workspace slug:

```rust
pub fn build_schema_name(org_id: &str, workspace_slug: &str) -> String {
    format!("{}_{}", org_id.replace('-', "_"), workspace_slug.replace('-', "_"))
}
```

See [Database Schema](./database-schema) for the full schema layout.

## Feature Flags

The worker crate supports optional features that can be enabled at compile time:

| Feature | Description | Enable With |
|---------|-------------|-------------|
| `kafka` | Kafka dispatcher support via `rdkafka` | `--features invokr-worker/kafka` |
| `redis-stream` | Redis Stream dispatcher support via `redis` | `--features invokr-worker/redis-stream` |
| `kms` | AWS KMS integration for secret encryption (in `invokr-common`) | `--features invokr-worker/kms` |
| `pg_cron` | pg_cron extension for CRON scheduling (database-level) | Enabled via migration |

```bash
# Build with Kafka support
cargo build --workspace --features invokr-worker/kafka

# Build with Redis Stream support
cargo build --workspace --features invokr-worker/redis-stream

# Build with all features
cargo build --workspace --features invokr-worker/kafka,invokr-worker/redis-stream,invokr-worker/kms
```

Kafka and Redis Stream dispatchers are conditionally compiled. When not enabled, the pipeline returns an `UNSUPPORTED_TYPE` error for those endpoint types. The `pg_cron` extension is installed at the database level via migration and is always available.

:::note
The `invokr-sdk` crate (generated Rust SDK) is excluded from the workspace build because it targets a different MSRV (1.82) and pulls a heavy AWS smithy runtime stack that the server crates don't need. See [Rust SDK](../sdks/rust) for details.
:::
