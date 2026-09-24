---
id: versioning
title: Job Versioning
---

# Job Versioning

CRON jobs in Invokr are **immutable**: they cannot be modified in place. Updating a CRON job instead creates a **new version** and retires the old one. This preserves a full audit trail of schedule changes without disrupting in-flight executions.

## How versioning works

When you `PUT /v1/jobs/{job_id}` to update a CRON job, Invokr performs a **retire-and-replace** operation:

1. A new job row is inserted with `version = previous_version + 1`.
2. The new job's `previous_version_id` points to the old job.
3. The old job's `status` is set to `RETIRED`.
4. The old job's `replaced_by_id` points to the new job.
5. The new job becomes `ACTIVE` and is registered with pg_cron.
6. The old job is unscheduled from pg_cron.

```
job_abc (v1, ACTIVE)
    └──→ PUT /v1/jobs/job_abc
            └──→ job_def (v2, ACTIVE)   ← previous_version_id: job_abc
                     job_abc (v1, RETIRED)  ← replaced_by_id: job_def
```

## Version fields

Each job row has three version-related fields:

| Field | Type | Description |
|-------|------|-------------|
| `version` | integer | Monotonically incrementing version number. Starts at `1`. |
| `previous_version_id` | string | ID of the job version this one replaced. `null` for the original (v1). |
| `replaced_by_id` | string | ID of the job version that replaced this one. `null` if this is the current/active version. |

## Updating a CRON job

```bash
curl -X PUT http://localhost:8080/v1/jobs/{job_id} \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "cron": "0 10 * * MON",
    "timezone": "Asia/Kolkata",
    "input": {
      "report_type": "weekly",
      "include_charts": true
    }
  }'
```

Response (`201 Created`):

```json
{
  "job_id": "job_e41b...",
  "endpoint": "generate-weekly-report",
  "endpoint_type": "HTTP",
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

:::note
The response returns `201 Created` (not `200 OK`) because a new job entity is created. The `job_id` is different from the original — it's a new row in the database.
:::

### What can be updated

You can update any of the following fields on a CRON job:

| Field | Description |
|-------|-------------|
| `cron` | New cron expression |
| `timezone` | New IANA timezone |
| `input` | New static input (used for every future tick) |
| `starts_at` | New start boundary |
| `ends_at` | New end boundary |

:::warning
Updating a CRON job with `PUT` is only supported for `CRON` trigger type. Attempting to update an `IMMEDIATE` or `DELAYED` job returns `409 JOB_NOT_UPDATABLE`.
:::

## Viewing version history

Retrieve the full version chain for a CRON job:

```bash
curl http://localhost:8080/v1/jobs/{job_id}/versions \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

The response returns all versions, **newest to oldest**, linked via `previous_version_id` and `replaced_by_id`. The version chain is reconstructed using a recursive CTE (Common Table Expression) that traverses the `previous_version_id` links.

```json
[
  {
    "job_id": "job_e41b...",
    "version": 2,
    "status": "ACTIVE",
    "cron": "0 10 * * MON",
    "timezone": "Asia/Kolkata",
    "previous_version_id": "job_c72f...",
    "created_at": "2026-03-15T11:00:00Z"
  },
  {
    "job_id": "job_c72f...",
    "version": 1,
    "status": "RETIRED",
    "cron": "0 9 * * MON",
    "timezone": "Asia/Kolkata",
    "replaced_by_id": "job_e41b...",
    "created_at": "2026-03-15T10:00:00Z"
  }
]
```

:::tip
You can request versions from **any** job ID in the chain — the API traverses the full chain regardless of which version you query. Querying `job_c72f` (v1) returns the same list as querying `job_e41b` (v2).
:::

## How the old version is retired

When a new version is created, the following happens atomically:

1. **Old job**: `status` → `RETIRED`, `replaced_by_id` → new job ID.
2. **Old pg_cron schedule**: unscheduled via `cron.unschedule()`.
3. **New job**: inserted with `status = ACTIVE`, `previous_version_id` → old job ID.
4. **New pg_cron schedule**: registered via `cron.schedule()`.
5. **In-flight executions**: continue to completion using the old job's configuration.

:::info
Executions that were already `QUEUED` or `RUNNING` when the update occurs are **not** affected. They dispatch using the endpoint configuration from when they were created. Only new executions (from future pg_cron ticks) use the updated schedule and input.
:::

## Version chain audit

The full version chain is preserved indefinitely for audit purposes. Each version retains:

- The exact `cron` expression and `timezone` at that version
- The `input` payload used for executions at that version
- The `created_at` timestamp
- The `status` (`ACTIVE` for the current version, `RETIRED` for previous versions)
- Links to previous and next versions

This enables:
- **Audit compliance**: Track exactly when schedules changed and what they were before.
- **Rollback**: Re-create a previous version's configuration by issuing another `PUT` with the old values.
- **Debugging**: Correlate execution failures with the version that was active at the time.

## Cancelling vs retiring

| Action | What happens | Status |
|--------|-------------|--------|
| `POST /jobs/{id}/cancel` | User-initiated stop. Unschedules from pg_cron. | `RETIRED` |
| `PUT /jobs/{id}` (update) | Creates new version. Old version unscheduled. | Old: `RETIRED`, New: `ACTIVE` |
| `cron_ends_at` reached | Reaper cleans up. Unschedules from pg_cron. | `RETIRED` |

In all cases, in-flight executions run to completion. The `RETIRED` status simply means no new executions will be created.

## Immutability of one-shot jobs

`IMMEDIATE` and `DELAYED` jobs are also immutable, but they cannot be updated at all — they fire once and complete. Attempting to `PUT` a one-shot job returns:

```json
{
  "error": {
    "code": "JOB_NOT_UPDATABLE",
    "message": "Cannot update one-shot jobs.",
    "request_id": "req_9a8b..."
  }
}
```

HTTP status: `409`.

## Full example: create, update, and view versions

```bash
# 1. Create a CRON job (v1)
CREATE_RESPONSE=$(curl -s -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "endpoint": "send-welcome-email",
    "trigger": "CRON",
    "cron": "0 9 * * MON",
    "timezone": "Asia/Kolkata",
    "input": { "report_type": "weekly" }
  }')

JOB_ID=$(echo "$CREATE_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin)['job_id'])")
echo "Created job (v1): $JOB_ID"

# 2. Update the CRON job (creates v2)
UPDATE_RESPONSE=$(curl -s -X PUT http://localhost:8080/v1/jobs/$JOB_ID \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "cron": "0 10 * * MON",
    "timezone": "Asia/Kolkata",
    "input": { "report_type": "weekly", "include_charts": true }
  }')

NEW_JOB_ID=$(echo "$UPDATE_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin)['job_id'])")
VERSION=$(echo "$UPDATE_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin)['version'])")
echo "Updated job (v$VERSION): $NEW_JOB_ID"

# 3. View the full version chain
curl -s http://localhost:8080/v1/jobs/$NEW_JOB_ID/versions \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" | python3 -m json.tool

# 4. Cancel the current version
curl -s -X POST http://localhost:8080/v1/jobs/$NEW_JOB_ID/cancel \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" | python3 -m json.tool
```

## See also

- [CRON jobs](./cron-jobs) — creating and scheduling recurring jobs
- [Pagination](./pagination) — listing jobs and executions
- [HTTP endpoints](./http-endpoints) — configuring delivery targets
- [Delayed jobs](./delayed-jobs) — one-shot scheduled execution
