---
id: delayed-jobs
title: Delayed Jobs
---

# Delayed Jobs

Delayed jobs fire once at a specific future time. When you create a delayed job, Invokr inserts an execution with `PENDING` status and a `run_at` timestamp. Workers pick it up automatically once `run_at <= now()`. No separate promoter process is needed.

## Creating a delayed job

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
    "input": { "order_id": "order-1234", "user_id": "u_abc" }
  }'
```

Response (`201 Created`):

```json
{
  "job_id": "job_8f3a...",
  "endpoint": "send-welcome-email",
  "endpoint_type": "HTTP",
  "trigger": "DELAYED",
  "status": "ACTIVE",
  "version": 1,
  "idempotency_key": "order-1234-reminder",
  "input": { "order_id": "order-1234", "user_id": "u_abc" },
  "execution": {
    "execution_id": "exec_2b7c...",
    "status": "PENDING",
    "created_at": "2026-03-15T10:00:00Z"
  },
  "created_at": "2026-03-15T10:00:00Z"
}
```

## How it works

Delayed jobs use **transaction-based pickup** rather than a separate promoter loop:

1. **Job creation**: The API inserts the job and an execution in a single transaction. The execution is created with status `PENDING` and the `run_at` timestamp from the request.

2. **Worker pickup**: The worker's claim query includes `PENDING` in its status filter:

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
   RETURNING ...;
   ```

3. **Execution**: Once `run_at <= now()`, the worker claims the execution, resolves templates, dispatches to the endpoint, and records the result.

:::tip
There is no separate "promoter" process that transitions `PENDING → QUEUED`. The worker's pickup query directly claims `PENDING` executions whose `run_at` has passed, so there is one fewer component to run and fail.
:::

## Pickup index

The `idx_executions_pickup` index covers the hot-path claim query:

```sql
CREATE INDEX idx_executions_pickup
    ON executions (status, run_at ASC)
    WHERE status IN ('QUEUED', 'RETRYING');
```

:::note
The partial index includes `QUEUED` and `RETRYING` statuses. `PENDING` executions are also covered by the worker's claim query (which filters on `status IN ('QUEUED', 'RETRYING', 'PENDING')`), ensuring delayed jobs are picked up efficiently when their time arrives.
:::

## ISO 8601 datetime format

The `run_at` field must be an **ISO 8601** datetime string. Always include the timezone offset (use `Z` for UTC):

```json
{
  "run_at": "2026-03-20T18:00:00Z"
}
```

Other valid formats:

| Format | Example |
|--------|---------|
| UTC | `2026-03-20T18:00:00Z` |
| With offset | `2026-03-20T18:00:00+05:30` |
| With offset | `2026-03-20T13:00:00-05:00` |

:::warning
Always specify a timezone. A datetime without an offset may be interpreted in the server's local timezone, leading to unexpected firing times.
:::

## Timing accuracy

Delayed jobs fire within approximately **200ms** of the specified `run_at` time. This is bounded by the worker's poll interval (`INVOKR_WORKER_POLL_INTERVAL_MS`, default 200ms):

| Component | Delay |
|-----------|-------|
| Worker poll interval | ~200ms |
| Template resolution | ~1-5ms (cache hit) |
| Dispatch | varies by endpoint type |
| **Total (typical)** | **~200ms after `run_at`** |

To reduce latency, decrease the poll interval:

```bash
INVOKR_WORKER_POLL_INTERVAL_MS=100 cargo run -p invokr-worker
```

:::info
Decreasing the poll interval increases database load. The default 200ms provides a good balance between latency and DB pressure for most workloads.
:::

## Job creation fields

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `endpoint` | string | yes | Name of a registered endpoint. |
| `trigger` | string | yes | Must be `DELAYED`. |
| `idempotency_key` | string | yes | Deduplication key. Must be unique per endpoint. |
| `run_at` | ISO 8601 | yes | When the job should fire. |
| `input` | object | no | Execution payload. Validated against the endpoint's payload spec. |

## Deduplication

Delayed jobs use the same deduplication mechanism as immediate jobs: a unique constraint on `(endpoint, idempotency_key)`. Duplicate requests return the existing job with `200 OK` instead of `201 Created`.

```bash
# First request — creates the job (201 Created)
curl -X POST http://localhost:8080/v1/jobs ... \
  -d '{ "endpoint": "send-welcome-email", "trigger": "DELAYED", "idempotency_key": "order-1234-reminder", "run_at": "2026-03-20T18:00:00Z", "input": {...} }'

# Duplicate request — returns existing job (200 OK)
curl -X POST http://localhost:8080/v1/jobs ... \
  -d '{ "endpoint": "send-welcome-email", "trigger": "DELAYED", "idempotency_key": "order-1234-reminder", "run_at": "2026-03-20T18:00:00Z", "input": {...} }'
```

## Polling for execution status

After creating a delayed job, poll the execution endpoint to check when it fires:

```bash
# Get execution details (status will be PENDING until run_at, then RUNNING/SUCCESS)
curl http://localhost:8080/v1/executions/{execution_id} \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

### Execution lifecycle for delayed jobs

```
PENDING ──→ (run_at arrives) ──→ RUNNING ──→ SUCCESS
                                    │
                                    ├──→ RETRYING ──→ RUNNING (next attempt)
                                    │
                                    └──→ FAILED (retries exhausted)
```

| Status | Meaning |
|--------|---------|
| `PENDING` | Execution created, waiting for `run_at` |
| `RUNNING` | Worker has claimed and is dispatching |
| `SUCCESS` | Dispatch succeeded (response in expected status codes) |
| `RETRYING` | Dispatch failed, waiting for backoff before next attempt |
| `FAILED` | All retry attempts exhausted |
| `CANCELLED` | Execution was cancelled while `PENDING` |

## Cancelling a delayed job

You can cancel a delayed job if the execution is still `PENDING` (hasn't fired yet):

```bash
curl -X POST http://localhost:8080/v1/jobs/{job_id}/cancel \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

:::warning
If the execution is already `RUNNING`, cancellation returns `409 EXECUTION_NOT_CANCELLABLE`. Once a delayed job starts executing, it cannot be stopped mid-flight.
:::

## Full example with polling

```bash
# 1. Create a delayed job that fires in 10 seconds
RUN_AT=$(date -u -v+10S +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || date -u -d "+10 seconds" +"%Y-%m-%dT%H:%M:%SZ")

JOB_RESPONSE=$(curl -s -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d "{
    \"endpoint\": \"send-welcome-email\",
    \"trigger\": \"DELAYED\",
    \"idempotency_key\": \"delayed-test-001\",
    \"run_at\": \"$RUN_AT\",
    \"input\": { \"order_id\": \"delayed-test\" }
  }")

EXECUTION_ID=$(echo "$JOB_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin)['execution']['execution_id'])")

echo "Execution ID: $EXECUTION_ID"

# 2. Poll until the execution completes
while true; do
  STATUS=$(curl -s http://localhost:8080/v1/executions/$EXECUTION_ID \
    -H "Authorization: Bearer dev-api-key" \
    -H "X-Org-Id: <org_id>" \
    -H "X-Workspace-Id: <workspace_id>" | python3 -c "import sys,json; print(json.load(sys.stdin).get('status','UNKNOWN'))")
  
  echo "Status: $STATUS"
  
  if [ "$STATUS" = "SUCCESS" ] || [ "$STATUS" = "FAILED" ]; then
    break
  fi
  
  sleep 1
done

echo "Final status: $STATUS"
```

## Testing delayed jobs

Run the delayed job integration test:

```bash
just test-delayed
```

This test creates a delayed job with a short delay, polls for completion, and verifies the execution succeeds.

## See also

- [HTTP endpoints](./http-endpoints) — configuring delivery targets
- [CRON jobs](./cron-jobs) — recurring schedules
- [Pagination](./pagination) — listing executions
- [Execution lifecycle](../core-concepts/executions) — full status transition diagram
