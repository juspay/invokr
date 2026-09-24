---
id: intro
title: Introduction
---

# Introduction

Invokr runs jobs for you and makes sure they actually happen. You hand it a job — send this HTTP request, publish to this Kafka topic, push to this Redis Stream — and Invokr delivers it: right now, at a set time, or on a repeating schedule.

The hard parts are handled for you. A job that has been accepted survives a process crash, because it is written to PostgreSQL before Invokr acknowledges it. A job that fails is retried with backoff. A job never runs twice, even if two workers reach for it at once. And every attempt is recorded, so you can always see what ran, when, and what came back.

Invokr is written in Rust and uses PostgreSQL — with the `pg_cron` extension — as its source of truth. There is no separate scheduler process or message broker to run alongside it.

---

## What you can do

| You want to… | The call |
|---|---|
| Fire a job now | `POST /v1/jobs` with `trigger: IMMEDIATE` |
| Fire one at a set time | `POST /v1/jobs` with `trigger: DELAYED` and a `run_at` |
| Fire one on a repeating schedule | `POST /v1/jobs` with `trigger: CRON` and a cron expression |
| Cancel one | `POST /v1/jobs/{id}/cancel` |

---

## Key guarantees

| Guarantee | How it's achieved |
|-----------|-------------------|
| **Exactly-once** | Idempotency keys + DB unique constraints + `SELECT FOR UPDATE SKIP LOCKED` |
| **Durable** | Every job persisted to PostgreSQL before acknowledgment |
| **Retry with backoff** | Configurable per endpoint: fixed, linear, or exponential with jitter |
| **Sub-second** | Immediate: ~300ms. Delayed: within ~200ms of `run_at` (worker poll interval) |
| **Observable** | Every execution has a lifecycle. Every attempt recorded with duration, output, and error |
| **Type-safe** | JSON Schema validation on job input at creation time |
| **Multi-tenant** | Schema-per-workspace isolation. Shared nothing between tenants |

---

## Architecture overview

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

### How scheduling works

Invokr uses **PostgreSQL pg_cron** for CRON materialization and **transaction-based pickup** for all job types. No separate scheduler process is needed — the database handles all scheduling concerns.

- **IMMEDIATE** jobs: Execution is created as `QUEUED` in the same transaction as the job. Workers pick it up directly.
- **DELAYED** jobs: Execution is created as `PENDING` with a `run_at` timestamp. Workers pick up PENDING executions once `run_at <= now()`.
- **CRON** jobs: Registered with pg_cron at creation time. pg_cron inserts a new `QUEUED` execution on each tick. Workers pick it up directly.

---

## Crates overview

Invokr is organized as a Cargo workspace with the following crates:

| Crate | Description |
|-------|-------------|
| `invokr-common` | Shared library — models, DB layer, config, tenant management, caching, metrics |
| `invokr-api` | REST API server (actix-web). CRUD for all resources, job invocation, Prometheus metrics at `/metrics` |
| `invokr-worker` | Execution engine. Polls DB for QUEUED/RETRYING/PENDING executions, resolves templates, dispatches to endpoints. Exposes metrics via HTTP listener |
| `invokr-mock-server` | Test fixture — HTTP server on port 9999 for integration tests |
| `invokr-dashboard` | Web UI — Leptos/WASM, shows jobs, executions, attempts. Excluded from workspace build |

---

## Multi-tenancy overview

Invokr uses **schema-per-tenant** isolation. Each workspace gets its own PostgreSQL schema with isolated tables. Shared tables live in the `public` schema.

```
public schema:        organizations, workspaces
tenant schema:        payload_specs, configs, secrets, endpoints,
(org_workspace):      jobs, executions, attempts, execution_logs
```

Tenant-scoped API requests require `X-Org-Id` and `X-Workspace-Id` headers. The worker iterates all active workspace schemas via a cached `SchemaRegistry` (30s TTL). See [Multi-Tenancy](./core-concepts/multi-tenancy) for details.

:::info
The schema-per-tenant model means each workspace has complete isolation — jobs, executions, endpoints, and all resources are scoped to the workspace's own database schema. Organizations live in the `public` schema and can contain multiple workspaces.
:::

---

## Deployment modes

Invokr runs in two deployment modes:

| Mode | Description | Use case |
|------|-------------|----------|
| **Library mode** (embedded) | Invokr embedded directly in your Rust application process. No HTTP overhead, no separate server. | Single Rust app that needs durable scheduling |
| **Service mode** (standalone) | Invokr runs as a standalone REST API. Multiple apps share one deployment. | Multiple apps, or decoupled operational lifecycle |

Both modes expose the same API through the `InvokrClient` trait. The [Quickstart](./quickstart) uses service mode. For library mode setup, see [Library Mode Setup](./deployment/library-mode). For the conceptual comparison, see [Dual Deployment Modes](./architecture/dual-deployment).

---

## Inspiration

The shape of the API owes a lot to JavaScript's `setTimeout` and `setInterval`: fire something now, fire it after a delay, or fire it over and over on a schedule. If you have reached for those, Invokr's three triggers — `IMMEDIATE`, `DELAYED`, and `CRON` — should feel familiar. What is different is everything underneath. A browser forgets its timers the moment the tab closes; an Invokr job is written down, retried when it fails, and kept as a record of what happened.

---

## Next steps

- [Quickstart](./quickstart) — get a job firing in under 5 minutes
- [Core Concepts](./core-concepts/overview) — understand the three-step workflow
- [Jobs](./core-concepts/jobs) — trigger types and job lifecycle
- [Executions](./core-concepts/executions) — execution lifecycle and retry behavior
