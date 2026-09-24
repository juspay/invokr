---
id: jobs
title: Jobs
---

# Jobs

A job is a single invocation of an endpoint. Creating one triggers execution — immediately, at a scheduled time, or on a recurring schedule. Jobs are the core primitive of Invokr: nearly everything you do comes down to creating a job and watching it run.

---

## Trigger types

A job has one of three trigger types:

| Trigger | Behavior |
|---------|----------|
| `IMMEDIATE` | Fires now. Execution created as `QUEUED` in the same transaction as the job. |
| `DELAYED` | Fires at a specific time. Execution created as `PENDING` with `run_at`. Workers pick it up when `run_at <= now()`. |
| `CRON` | Fires repeatedly on a schedule. Registered with pg_cron at creation time. Each tick inserts a new `QUEUED` execution. |

---

## Job fields

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `job_id` | string | auto | Unique identifier (UUID). Auto-generated on creation. |
| `endpoint` | string | yes | Name of a registered endpoint. |
| `trigger` | string | yes | `IMMEDIATE`, `DELAYED`, or `CRON`. |
| `status` | string | auto | `ACTIVE` or `RETIRED`. Defaults to `ACTIVE`. |
| `version` | integer | auto | Version number. Starts at 1. Increments on CRON job updates. |
| `idempotency_key` | string | yes* | Deduplication key. *Required for `IMMEDIATE` and `DELAYED`. System-generated for `CRON` (`cron_{job_id}_{epoch_ms}`). |
| `input` | object | no | Job input payload. Validated against endpoint's payload spec at creation time. Static for `CRON` (used for every tick). |
| `run_at` | ISO 8601 | no | Required for `DELAYED`. The timestamp when the job should fire. |
| `cron` | string | no | Required for `CRON`. 5-field cron expression (e.g. `0 9 * * MON`). |
| `timezone` | string | no | Required for `CRON`. IANA timezone (e.g. `Asia/Kolkata`). |
| `starts_at` | ISO 8601 | no | Optional for `CRON`. When the schedule begins. Default: now. |
| `ends_at` | ISO 8601 | no | Optional for `CRON`. When the schedule ends. `null` = indefinite. |
| `previous_version_id` | string | auto | For CRON version chains. The job ID of the previous version. |
| `replaced_by_id` | string | auto | For CRON version chains. The job ID that replaced this one. |
| `created_at` | ISO 8601 | auto | Creation timestamp. |
| `retired_at` | ISO 8601 | auto | When the job was retired (cancelled or replaced). |

---

## Job status

Jobs have two statuses:

| Status | Description |
|--------|-------------|
| `ACTIVE` | The job is live. One-shot jobs are active until they complete. CRON jobs are active until cancelled or retired. |
| `RETIRED` | The job has been cancelled or replaced by a newer version. No new executions will be created. |

:::note
For one-shot jobs (`IMMEDIATE`, `DELAYED`), the job remains `ACTIVE` even after its execution completes. The job record is a permanent record of what was invoked. For `CRON` jobs, retiring stops future executions but in-flight executions run to completion.
:::

---

## Creating jobs

### Immediate job

Fires now — execution is created as `QUEUED` in the same transaction:

```bash
curl -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "IMMEDIATE",
    "idempotency_key": "order-1234-welcome",
    "input": { "order_id": "order-1234", "user_id": "u_abc" }
  }'
```

Response (`201 Created`):

```json
{
  "job_id": "job_8f3a...",
  "endpoint": "send-welcome-email",
  "endpoint_type": "HTTP",
  "trigger": "IMMEDIATE",
  "status": "ACTIVE",
  "version": 1,
  "idempotency_key": "order-1234-welcome",
  "input": { "order_id": "order-1234", "user_id": "u_abc" },
  "execution": {
    "execution_id": "exec_2b7c...",
    "status": "QUEUED",
    "created_at": "2026-03-15T10:00:00Z"
  },
  "created_at": "2026-03-15T10:00:00Z"
}
```

### Delayed job

Fires at a specific time — execution is created as `PENDING` with `run_at`:

```bash
curl -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "DELAYED",
    "idempotency_key": "order-1234-reminder",
    "run_at": "2026-03-20T18:00:00Z",
    "input": { "order_id": "order-1234" }
  }'
```

### CRON job

Fires on a recurring schedule — registered with pg_cron at creation time:

```bash
curl -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "CRON",
    "cron": "0 9 * * MON",
    "timezone": "Asia/Kolkata",
    "input": { "order_id": "all" }
  }'
```

Response (`201 Created`):

```json
{
  "job_id": "job_c72f...",
  "endpoint": "send-welcome-email",
  "endpoint_type": "HTTP",
  "trigger": "CRON",
  "status": "ACTIVE",
  "version": 1,
  "cron": "0 9 * * MON",
  "timezone": "Asia/Kolkata",
  "next_run_at": "2026-03-16T09:00:00+05:30",
  "input": { "order_id": "all" },
  "created_at": "2026-03-15T10:00:00Z"
}
```

---

## Job immutability and versioning

Jobs are **immutable**. One-shot jobs (`IMMEDIATE`, `DELAYED`) fire and complete — they cannot be updated. Attempting to update a one-shot job returns `409 JOB_NOT_UPDATABLE`.

CRON jobs are "updated" by creating a **new version** and retiring the old one. The full version chain is preserved for audit:

```
job_abc (v1, ACTIVE)
    └──→ PUT /v1/jobs/job_abc
            └──→ job_def (v2, ACTIVE)  ← previous_version_id: job_abc
                     job_abc (v1, RETIRED)
```

### Updating a CRON job

```bash
curl -X PUT http://localhost:8080/v1/jobs/{job_id} \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "cron": "0 */2 * * *",
    "input": { "mode": "v2" }
  }'
```

Response includes `version: 2`, `previous_version_id`, and the old job is set to `status: RETIRED`.

### Viewing version history

```bash
curl http://localhost:8080/v1/jobs/{job_id}/versions \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

---

## Cancelling jobs

```bash
curl -X POST http://localhost:8080/v1/jobs/{job_id}/cancel \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

Cancel behavior depends on the trigger type:

| Trigger | Cancel behavior |
|---------|----------------|
| `CRON` | Sets `status = RETIRED`, stops future executions. In-flight executions run to completion. |
| `IMMEDIATE` / `DELAYED` | Cancels execution if `PENDING` or `QUEUED`. Returns `409 EXECUTION_NOT_CANCELLABLE` if already `RUNNING`. |

---

## Job health status

For CRON jobs, the status endpoint provides a health overview:

```bash
curl http://localhost:8080/v1/jobs/{job_id}/status \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

Response:

```json
{
  "job_id": "job_c72f...",
  "job_status": "ACTIVE",
  "latest_execution": {
    "execution_id": "exec_8f3a...",
    "status": "SUCCESS",
    "attempt_count": 1,
    "max_attempts": 3,
    "started_at": "2026-03-15T10:30:00Z",
    "completed_at": "2026-03-15T10:30:01Z"
  }
}
```

---

## See also

- [Executions](./executions) — the execution lifecycle and state machine
- [Endpoints](./endpoints) — what jobs invoke
- [Idempotency](./idempotency) — how duplicate jobs are deduplicated
- [Payload Specs](./payload-specs) — input validation
- [Versioning Guide](../guides/versioning) — CRON version chains in depth
