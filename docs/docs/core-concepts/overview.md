---
id: overview
title: The Three-Step Workflow
---

# The Three-Step Workflow

Invokr organizes work into a three-step model: **Setup**, **Register**, and **Invoke**. Each step builds on the previous one, creating a clean separation between defining contracts, configuring delivery targets, and firing jobs.

---

## The three steps

| Step | What you do | Resources | Endpoints |
|------|-------------|-----------|----------|
| **1. Setup** | Create input contracts, configs, and secrets | Payload Specs, Configs, Secrets | `/v1/payload-specs`, `/v1/configs`, `/v1/secrets` |
| **2. Register** | Define where and how to deliver | Endpoints | `/v1/endpoints` |
| **3. Invoke** | Fire a job — now, later, or on a schedule | Jobs, Executions | `/v1/jobs` |

---

## How resources reference each other

Resources in Invokr form a dependency chain. An **endpoint** references a **payload spec** (for input validation), a **config** (for static variables), and optionally **secrets** (referenced within endpoint spec templates). A **job** references an **endpoint** by name.

```
┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│ Payload Spec │     │    Config    │     │    Secret    │
│ (JSON Schema)│     │  (variables) │     │ (encrypted)  │
└──────┬───────┘     └──────┬───────┘     └──────┬───────┘
       │                    │                    │
       │    referenced by   │                    │
       └────────────────────┼────────────────────┘
                            │
                    ┌───────▼───────┐
                    │   Endpoint     │
                    │ (delivery def) │
                    │  - type        │
                    │  - spec        │
                    │  - retry_policy│
                    └───────┬───────┘
                            │
                    invoked by
                            │
                    ┌───────▼───────┐
                    │     Job       │
                    │  - trigger    │
                    │  - input      │
                    │  - idempotency │
                    └───────┬───────┘
                            │
                    produces
                            │
                    ┌───────▼───────┐
                    │  Execution    │
                    │  - status     │
                    │  - attempts   │
                    └───────────────┘
```

### Step 1: Setup

Before you can deliver anything, you define the supporting resources:

- **Payload Specs** — JSON Schemas that define the input contract for an endpoint. When an endpoint references a payload spec, every job's input is validated against the schema at creation time. See [Payload Specs](./payload-specs).
- **Configs** — Key-value objects holding static variables (base URLs, topic names, feature flags). Referenced by endpoints and resolved at execution runtime via `{{config.*}}` templates. See [Configs](./configs).
- **Secrets** — Sensitive values (API keys, credentials) encrypted at rest with AES-256-GCM. Referenced via `{{secret.*}}` templates. Write-only — values are never returned in API responses. See [Secrets](./secrets).

### Step 2: Register

An **endpoint** is a registered delivery target. It defines:
- **Type**: `HTTP`, `KAFKA`, `REDIS_STREAM`, or `INTERNAL`
- **Payload spec reference**: for input validation
- **Config reference**: for static variables available as `{{config.*}}`
- **Spec**: transport-specific configuration (URL, method, headers, body template for HTTP; bootstrap servers, topic, key/value templates for Kafka; etc.)
- **Retry policy**: how failures should be retried (backoff strategy, max attempts, delays)

See [Endpoints](./endpoints) for details.

### Step 3: Invoke

A **job** is an invocation of an endpoint. Creating a job triggers execution:

- **IMMEDIATE** — fires now (like `setTimeout(fn, 0)`)
- **DELAYED** — fires at a specific time (like `setTimeout(fn, delay)`)
- **CRON** — fires on a recurring schedule (like `setInterval(fn, interval)`)

Each job fire produces an **execution** — a single delivery attempt with a lifecycle: `PENDING → QUEUED → RUNNING → SUCCESS / RETRYING / FAILED / CANCELLED`. Failed attempts retry per the endpoint's retry policy. See [Jobs](./jobs) and [Executions](./executions).

---

## Template resolution

Endpoint specs use template variables resolved from four namespaces at execution time:

| Namespace | Source | Example |
|-----------|--------|---------|
| `{{input.*}}` | Per-job input payload | `{{input.user_id}}` → `"u_abc"` |
| `{{config.*}}` | Endpoint's referenced config | `{{config.api_base_url}}` → `"https://api.myapp.com"` |
| `{{secret.*}}` | Encrypted secret store | `{{secret.email_api_key}}` → resolved at runtime, never exposed |
| `{{execution.*}}` | Execution metadata | `{{execution.idempotency_key}}` → the execution's idempotency key |

Templates are resolved by the worker at execution time. If any variable is unresolvable, the execution fails immediately — no wasted retries. See [Templates](./templates) for the full resolution engine details.

---

## Cross-references

- [Jobs](./jobs) — trigger types, fields, and lifecycle
- [Executions](./executions) — execution state machine and attempts
- [Endpoints](./endpoints) — endpoint types and configuration
- [Payload Specs](./payload-specs) — input validation with JSON Schema
- [Configs](./configs) — static variables and caching
- [Secrets](./secrets) — encryption and write-only access
- [Templates](./templates) — the template resolution engine
- [Retry Policy](./retry-policy) — backoff strategies and jitter
- [Idempotency](./idempotency) — de-duplication keys and what they do (and don't) guarantee
- [Multi-Tenancy](./multi-tenancy) — schema-per-workspace isolation
