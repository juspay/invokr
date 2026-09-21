# Invokr — API Specification

**Version:** 4.1.0
**Date:** March 15, 2026

---

## Overview

Invokr is a distributed job scheduling and execution engine. It provides durable, de-duplicated, retriable execution of HTTP callbacks — with support for immediate, delayed, and recurring triggers. Scheduling is exactly-once; delivery is at-least-once, so callbacks should be idempotent.

**Base URL:** `https://api.invokr.io/v1`

### How It Works

| Step | What you do | Endpoint |
|------|------------|----------|
| **1. Setup** | Create configs, secrets, and input schemas | `/configs`, `/schemas`, `/secrets` |
| **2. Register** | Register an HTTP callback | `POST /callbacks` |
| **3. Invoke** | Fire the callback — now, later, or on a schedule | `POST /jobs` |

### Conceptual Model

| JS Primitive | Invokr | Trigger |
|---|---|---|
| `setTimeout(fn, 0)` | `POST /jobs { trigger: IMMEDIATE }` | Fire once, now |
| `setTimeout(fn, delay)` | `POST /jobs { trigger: DELAYED }` | Fire once, later |
| `setInterval(fn, interval)` | `POST /jobs { trigger: CRON }` | Fire repeatedly |
| `clearTimeout` / `clearInterval` | `POST /jobs/{id}/cancel` | Stop |

**What the platform adds:** durability, distributed execution, retry with backoff, exactly-once scheduling (at-least-once delivery), and full observability.

---

## Core Concepts

### Domain Model

| Entity | Description |
|--------|-------------|
| **Schema** | A JSON Schema defining the input contract for a callback. Validated at job creation time. |
| **Config** | A key-value object holding static variables (base URLs, feature flags). Referenced by callbacks, resolved at execution runtime. |
| **Secret** | A sensitive value (API key, credential). Referenced via `{{secret.*}}`. Resolved at runtime, never exposed in responses. |
| **Callback** | A registered HTTP endpoint. Defines the URL, method, headers, body template, and retry policy. Created once, invoked by many jobs. |
| **Job** | An invocation of a callback. Creating a job triggers execution. One-shot jobs (`IMMEDIATE`, `DELAYED`) fire once. Persistent jobs (`CRON`) generate executions on a schedule until cancelled. |
| **Execution** | A single HTTP request made by the system. Each job fire produces one execution. *(API deferred to later)* |
| **Attempt** | A single try within an execution. Failed attempts retry per the callback's retry policy. *(API deferred to later)* |

### Template Resolution

Callback specs use template variables resolved from three namespaces:

| Namespace | Source | Resolved when | Per-execution? |
|---|---|---|---|
| `{{input.*}}` | Job input | Execution runtime | Yes |
| `{{config.*}}` | Callback's referenced config | Execution runtime | No |
| `{{secret.*}}` | Secret store | Execution runtime | No |

### Deduplication

Single mechanism: **unique constraint on `(callback, idempotency_key)`**.

| Trigger | Key provided by | Example |
|---------|----------------|---------|
| `IMMEDIATE` / `DELAYED` | Client | `order-1234-welcome-email` |
| `CRON` | System | `job_{id}_{epoch_ms}` |

Duplicate requests return the existing entity with `200 OK` instead of `201 Created`.

### Immutability (Jobs)

Jobs are **immutable**. One-shot jobs fire and complete. `CRON` jobs are updated by creating a new version and retiring the old one, linked via `previous_version_id`.

```
job_abc (v1, ACTIVE)
    └──→ PUT /jobs/job_abc
            └──→ job_def (v2, ACTIVE)  ← previous_version_id: job_abc
                     job_abc (v1, RETIRED)
```

---

## Authentication

```
Authorization: Bearer <api_key>
```

---

## Step 1 — Setup

### Schemas

Input schemas define the contract for job input. Stored as JSON Schema. When a callback references a schema, all job input is validated against it before execution.

#### Create a Schema

```
POST /schemas
```

```json
{
  "name": "send-welcome-email-input",
  "schema": {
    "type": "object",
    "properties": {
      "user_id": { "type": "string", "description": "Target user ID" },
      "order_id": { "type": "string", "description": "Associated order ID" }
    },
    "required": ["user_id"]
  }
}
```

**Response — `201 Created`:**

```json
{
  "name": "send-welcome-email-input",
  "schema": { ... },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

#### Other Schema Endpoints

```
GET    /schemas              List all schemas
GET    /schemas/{name}       Get a schema
PUT    /schemas/{name}       Update a schema
DELETE /schemas/{name}       Delete (fails if callbacks reference it)
```

> **Note:** Updating a schema does not affect running executions. The updated schema applies to future job creations only.

---

### Configs

Configs hold static variables used across executions of a callback. Values are available in callback specs as `{{config.*}}`.

#### Create a Config

```
POST /configs
```

```json
{
  "name": "email-service",
  "values": {
    "api_base_url": "https://api.myapp.com",
    "sender": "noreply@myapp.com",
    "max_retries": 3
  }
}
```

**Response — `201 Created`:**

```json
{
  "name": "email-service",
  "values": { ... },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

#### Other Config Endpoints

```
GET    /configs              List all configs
GET    /configs/{name}       Get a config
PUT    /configs/{name}       Update a config
DELETE /configs/{name}       Delete (fails if callbacks reference it)
```

> **Note:** Config updates take effect for future executions. In-flight executions use the config snapshot from when they started.

---

### Secrets

Secrets hold sensitive values. Referenced in callback specs via `{{secret.*}}`. Values are write-only — never returned in API responses.

#### Create a Secret

```
POST /secrets
```

```json
{
  "name": "email_api_key",
  "value": "sk-live-abc123..."
}
```

**Response — `201 Created`:**

```json
{
  "name": "email_api_key",
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

Note: `value` is never returned.

#### Other Secret Endpoints

```
GET    /secrets              List all secrets (names only, no values)
GET    /secrets/{name}       Get secret metadata (no value)
PUT    /secrets/{name}       Rotate / update a secret value
DELETE /secrets/{name}       Delete (fails if callbacks reference it)
```

---

## Step 2 — Register

### `POST /callbacks`

Register an HTTP callback. Defines the endpoint to call, retry behavior, and references to schemas and configs.

**Request:**

```json
{
  "name": "send-welcome-email",
  "schema": "send-welcome-email-input",
  "config": "email-service",
  "spec": {
    "url": "{{config.api_base_url}}/emails/welcome",
    "method": "POST",
    "headers": {
      "Authorization": "Bearer {{secret.email_api_key}}"
    },
    "body_template": {
      "user_id": "{{input.user_id}}",
      "sender": "{{config.sender}}"
    },
    "timeout_ms": 5000,
    "expected_status_codes": [200, 201, 202, 204]
  },
  "retry_policy": {
    "max_attempts": 3,
    "backoff": "exponential",
    "initial_delay_ms": 1000,
    "max_delay_ms": 30000,
    "retry_on_status_codes": [500, 502, 503, 504]
  }
}
```

**Response — `201 Created`:**

```json
{
  "name": "send-welcome-email",
  "schema": "send-welcome-email-input",
  "config": "email-service",
  "spec": { ... },
  "retry_policy": { ... },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

> **Validation:** `schema` and `config` references are verified at registration time. Invalid references → `422`.

### Other Callback Endpoints

```
GET    /callbacks              List all callbacks
GET    /callbacks/{name}       Get a callback
PUT    /callbacks/{name}       Update (applies to future jobs only)
DELETE /callbacks/{name}       Delete (fails if active jobs reference it)
```

### Callback — Field Reference

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `name` | string | ✓ | Unique identifier. URL-safe (lowercase alphanumeric, hyphens). |
| `schema` | string | | Name of a registered schema. Enables input validation. |
| `config` | string | | Name of a registered config. Values available as `{{config.*}}`. |
| `spec` | object | ✓ | HTTP endpoint definition. See below. |
| `retry_policy` | object | | Retry behavior on failure. See below. |

#### `spec`

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `url` | string | ✓ | Target URL. Supports `{{config.*}}`, `{{secret.*}}`. |
| `method` | string | ✓ | `GET`, `POST`, `PUT`, `PATCH`, `DELETE`. |
| `headers` | map | | Key-value pairs. Supports `{{config.*}}`, `{{secret.*}}`. |
| `body_template` | object | | JSON body. Supports `{{input.*}}`, `{{config.*}}`, `{{secret.*}}`. |
| `timeout_ms` | integer | ✓ | Request timeout in milliseconds. |
| `expected_status_codes` | integer[] | | Status codes treated as success. Default: `[200, 201, 202, 204]`. |

#### `retry_policy`

| Field | Type | Required | Default | Description |
|-------|------|:--------:|:-------:|-------------|
| `max_attempts` | integer | | `1` | Total attempts including first. `1` = no retries. |
| `backoff` | string | | `exponential` | `fixed`, `linear`, `exponential`. |
| `initial_delay_ms` | integer | | `1000` | Delay before first retry. |
| `max_delay_ms` | integer | | `60000` | Upper bound on backoff. |
| `retry_on_status_codes` | integer[] | | `[500, 502, 503, 504]` | Status codes that trigger retry. |

---

## Step 3 — Invoke

### `POST /jobs`

Invoke a callback. Specify when to fire and with what input.

**Immediate — `setTimeout(fn, 0)`:**

```json
{
  "callback": "send-welcome-email",
  "trigger": "IMMEDIATE",
  "idempotency_key": "order-1234-welcome-email",
  "input": {
    "user_id": "u_abc",
    "order_id": "order-1234"
  }
}
```

**Delayed — `setTimeout(fn, delay)`:**

```json
{
  "callback": "send-welcome-email",
  "trigger": "DELAYED",
  "idempotency_key": "order-1234-reminder",
  "run_at": "2026-03-15T18:00:00Z",
  "input": {
    "user_id": "u_abc",
    "order_id": "order-1234"
  }
}
```

**Cron — `setInterval(fn, interval)`:**

```json
{
  "callback": "generate-weekly-report",
  "trigger": "CRON",
  "cron": "0 9 * * MON",
  "timezone": "Asia/Kolkata",
  "starts_at": "2026-03-16T00:00:00Z",
  "ends_at": null,
  "input": {
    "report_type": "weekly"
  }
}
```

### Responses

**`201 Created` — Immediate / Delayed:**

```json
{
  "job_id": "job_8f3a...",
  "callback": "send-welcome-email",
  "trigger": "IMMEDIATE",
  "status": "ACTIVE",
  "version": 1,
  "idempotency_key": "order-1234-welcome-email",
  "input": { "user_id": "u_abc", "order_id": "order-1234" },
  "execution": {
    "execution_id": "exec_2b7c...",
    "status": "QUEUED",
    "created_at": "2026-03-15T10:00:00Z"
  },
  "created_at": "2026-03-15T10:00:00Z"
}
```

**`201 Created` — Cron:**

```json
{
  "job_id": "job_c72f...",
  "callback": "generate-weekly-report",
  "trigger": "CRON",
  "status": "ACTIVE",
  "version": 1,
  "cron": "0 9 * * MON",
  "timezone": "Asia/Kolkata",
  "starts_at": "2026-03-16T00:00:00Z",
  "ends_at": null,
  "next_run_at": "2026-03-16T09:00:00+05:30",
  "input": { "report_type": "weekly" },
  "created_at": "2026-03-15T10:00:00Z"
}
```

**`200 OK`** — Deduplicated, returns existing job.

### Job Creation — Field Reference

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `callback` | string | ✓ | Name of a registered callback. |
| `trigger` | string | ✓ | `IMMEDIATE`, `DELAYED`, `CRON`. |
| `idempotency_key` | string | ✓* | Deduplication key. *Required for `IMMEDIATE` and `DELAYED`. |
| `input` | object | | Execution payload. Validated against callback's schema. Static for `CRON` (used for every tick). |
| `run_at` | ISO 8601 | | Required for `DELAYED`. |
| `cron` | string | | Required for `CRON`. 5-field cron expression. |
| `timezone` | string | | Required for `CRON`. IANA timezone. |
| `starts_at` | ISO 8601 | | Optional for `CRON`. Default: now. |
| `ends_at` | ISO 8601 | | Optional for `CRON`. `null` = indefinite. |

---

## Job Lifecycle

### `GET /jobs/{job_id}`

Get full job details.

### `PUT /jobs/{job_id}` — Update (CRON only)

Creates a new version, retires the old one. Returns `409 JOB_NOT_UPDATABLE` for one-shot jobs.

**Request:**

```json
{
  "cron": "0 10 * * MON",
  "timezone": "Asia/Kolkata",
  "input": {
    "report_type": "weekly",
    "include_charts": true
  }
}
```

**Response — `201 Created`:**

```json
{
  "job_id": "job_e41b...",
  "callback": "generate-weekly-report",
  "trigger": "CRON",
  "status": "ACTIVE",
  "version": 2,
  "previous_version_id": "job_c72f...",
  "cron": "0 10 * * MON",
  "timezone": "Asia/Kolkata",
  "next_run_at": "2026-03-16T10:00:00+05:30",
  "input": { "report_type": "weekly", "include_charts": true },
  "created_at": "2026-03-15T11:00:00Z"
}
```

### `POST /jobs/{job_id}/cancel` — clearTimeout / clearInterval

**CRON jobs:** sets status to `RETIRED`, stops future executions. In-flight executions run to completion.

**One-shot jobs:** cancels execution if `PENDING` or `QUEUED`. Returns `409` if already `RUNNING`.

**Response — `200 OK`:**

```json
{
  "job_id": "job_c72f...",
  "status": "RETIRED",
  "retired_at": "2026-03-15T12:00:00Z"
}
```

### `GET /jobs/{job_id}/versions`

Full version chain for CRON jobs, newest to oldest.

### `GET /jobs/{job_id}/status`

Health overview for CRON jobs.

```json
{
  "job_id": "job_c72f...",
  "callback": "generate-weekly-report",
  "trigger": "CRON",
  "health": "HEALTHY",
  "version": 1,

  "last_execution": {
    "execution_id": "exec_8f3a...",
    "status": "SUCCESS",
    "started_at": "2026-03-15T10:30:00Z",
    "completed_at": "2026-03-15T10:30:01Z",
    "attempt_number": 1
  },

  "active_executions": {
    "pending": 2,
    "running": 1,
    "total": 3
  },

  "cron": {
    "expression": "0 9 * * MON",
    "next_run_at": "2026-03-16T09:00:00+05:30",
    "last_tick_at": "2026-03-09T09:00:00+05:30"
  },

  "stats": {
    "last_24h": {
      "total": 142,
      "succeeded": 139,
      "failed": 3,
      "avg_duration_ms": 340,
      "p99_duration_ms": 1200
    }
  }
}
```

| Health | Condition |
|--------|-----------|
| `HEALTHY` | Recent executions mostly succeeding. |
| `DEGRADED` | Elevated failure rate, but some succeeding. |
| `FAILING` | Most recent executions failing. |
| `IDLE` | No executions in recent window. |

### `GET /jobs`

| Param | Type | Default | Description |
|-------|------|:-------:|-------------|
| `callback` | string | | Filter by callback name. |
| `trigger` | string | | Filter: `IMMEDIATE`, `DELAYED`, `CRON`. |
| `status` | string | | Filter: `ACTIVE`, `RETIRED`. |
| `from` | ISO 8601 | | Start of time range. |
| `to` | ISO 8601 | | End of time range. |
| `limit` | integer | `50` | Page size. Max `200`. |
| `cursor` | string | | Pagination cursor. |

---

## Error Responses

```json
{
  "error": {
    "code": "JOB_NOT_FOUND",
    "message": "Job with ID 'job_xyz' does not exist.",
    "request_id": "req_9a8b..."
  }
}
```

| HTTP Status | Code | Description |
|:-----------:|------|-------------|
| `400` | `INVALID_REQUEST` | Malformed request body. |
| `401` | `UNAUTHORIZED` | Missing or invalid API key. |
| `404` | `SCHEMA_NOT_FOUND` | Schema name does not exist. |
| `404` | `CONFIG_NOT_FOUND` | Config name does not exist. |
| `404` | `SECRET_NOT_FOUND` | Secret name does not exist. |
| `404` | `CALLBACK_NOT_FOUND` | Callback name does not exist. |
| `404` | `JOB_NOT_FOUND` | Job ID does not exist. |
| `404` | `EXECUTION_NOT_FOUND` | Execution ID does not exist. |
| `409` | `CONFLICT` | Cannot delete resource with active dependents. |
| `409` | `JOB_NOT_UPDATABLE` | Cannot update one-shot jobs. |
| `409` | `EXECUTION_NOT_CANCELLABLE` | Execution already running or completed. |
| `422` | `INVALID_CRON` | Invalid cron expression. |
| `422` | `INVALID_SCHEMA` | Schema JSON is not valid JSON Schema. |
| `422` | `INVALID_SCHEMA_REF` | Callback's schema reference not found. |
| `422` | `INVALID_CONFIG_REF` | Callback's config reference not found. |
| `422` | `INPUT_VALIDATION_FAILED` | Input does not match schema. |
| `422` | `TEMPLATE_RESOLUTION_FAILED` | Template variable unresolvable at runtime. |
| `429` | `RATE_LIMITED` | Too many requests. |
| `500` | `INTERNAL_ERROR` | Unexpected server error. |

---

## Rate Limits

| Endpoint | Limit |
|----------|-------|
| `POST /jobs` | 1000 req/s per callback |
| `GET /jobs/{job_id}/status` | 100 req/s per job |
| All other endpoints | 200 req/s per API key |

```
X-RateLimit-Limit: 1000
X-RateLimit-Remaining: 997
X-RateLimit-Reset: 1742108400
```

---

## Endpoint Summary

### Step 1 — Setup

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/schemas` | Create a schema |
| `GET` | `/schemas` | List schemas |
| `GET` | `/schemas/{name}` | Get a schema |
| `PUT` | `/schemas/{name}` | Update a schema |
| `DELETE` | `/schemas/{name}` | Delete a schema |
| `POST` | `/configs` | Create a config |
| `GET` | `/configs` | List configs |
| `GET` | `/configs/{name}` | Get a config |
| `PUT` | `/configs/{name}` | Update a config |
| `DELETE` | `/configs/{name}` | Delete a config |
| `POST` | `/secrets` | Create a secret |
| `GET` | `/secrets` | List secrets (names only) |
| `GET` | `/secrets/{name}` | Get secret metadata |
| `PUT` | `/secrets/{name}` | Rotate a secret |
| `DELETE` | `/secrets/{name}` | Delete a secret |

### Step 2 — Register

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/callbacks` | Register a callback |
| `GET` | `/callbacks` | List callbacks |
| `GET` | `/callbacks/{name}` | Get a callback |
| `PUT` | `/callbacks/{name}` | Update a callback |
| `DELETE` | `/callbacks/{name}` | Delete a callback |

### Step 3 — Invoke

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/jobs` | Create a job (triggers execution) |
| `GET` | `/jobs` | List jobs |
| `GET` | `/jobs/{job_id}` | Get a job |
| `PUT` | `/jobs/{job_id}` | Update CRON job (new version) |
| `POST` | `/jobs/{job_id}/cancel` | Cancel / retire a job |
| `GET` | `/jobs/{job_id}/status` | Job health and stats |
| `GET` | `/jobs/{job_id}/versions` | Version history |
