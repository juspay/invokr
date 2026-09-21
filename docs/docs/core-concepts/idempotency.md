---
id: idempotency
title: Idempotency
---

# Idempotency

Idempotency keys are how Invokr avoids *scheduling* the same job twice: every job carries one, and the database enforces uniqueness on it. They are also how your receivers avoid *processing* the same delivery twice — Invokr's delivery is at-least-once, and it hands the key to the target so it can de-duplicate. See [Delivery Guarantees](../architecture/exactly-once) for the distinction.

---

## What are idempotency keys?

An idempotency key is a client-provided (or system-generated) string that uniquely identifies a job invocation. When two requests share the same idempotency key for the same endpoint, the second request is treated as a duplicate and returns the original entity instead of creating a new one.

| Trigger | Key provided by | Example |
|---------|-----------------|---------|
| `IMMEDIATE` / `DELAYED` | Client | `order-1234-welcome-email` |
| `CRON` | System | `cron_{job_id}_{epoch_ms}` |

:::info
Idempotency keys are **required** for `DELAYED` jobs. They are optional for `IMMEDIATE` jobs — Invokr generates a UUID when you omit one, which means an omitted key gives you no de-duplication at all. For `CRON` jobs the system generates one per tick.
:::

---

## DB unique constraints

Invokr enforces idempotency at the database level with two unique indexes:

### `idx_jobs_idempotency` — Job-level dedup

```sql
CREATE UNIQUE INDEX idx_jobs_idempotency
    ON jobs (endpoint, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

This index ensures that only one job can exist per `(endpoint, idempotency_key)` pair. When a duplicate `POST /v1/jobs` request arrives with the same endpoint and idempotency key, the `INSERT` fails with a unique constraint violation. The API catches this and returns the existing job with `200 OK` instead of `201 Created`.

### `idx_executions_cron_dedup` — Execution-level dedup for CRON

```sql
CREATE UNIQUE INDEX idx_executions_cron_dedup
    ON executions (job_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

This index ensures that only one execution can exist per `(job_id, idempotency_key)` pair. This is critical for CRON tick deduplication — if pg_cron fires the same tick twice (e.g. due to a retry), the second execution insert is a no-op (`ON CONFLICT DO NOTHING`).

---

## Duplicate requests return existing entity

When a duplicate job creation request is received:

| Scenario | HTTP Status | Behavior |
|----------|:-----------:|---------|
| First request (new idempotency key) | `201 Created` | New job and execution are created |
| Duplicate request (same endpoint + idempotency key) | `200 OK` | Existing job is returned, no new execution created |

:::tip
The `200 OK` vs `201 Created` distinction lets clients detect whether a job was newly created or was a duplicate. This is useful for logging, metrics, and client-side caching.
:::

---

## CRON tick deduplication

For `CRON` jobs, idempotency keys are system-generated for each tick:

```
cron_{job_id}_{epoch_ms}
```

Where `epoch_ms` is the Unix epoch millisecond timestamp of the CRON tick. This ensures:

1. **Each tick has a unique key** — `cron_job_abc_1742025600000` for the 09:00:00 UTC tick
2. **Duplicate ticks are deduplicated** — if pg_cron fires the same tick twice, the `INSERT ... ON CONFLICT DO NOTHING` ensures only one execution is created
3. **Missed ticks are caught up** — the scheduler computes `next_run_at` from the current tick, not `now()`, so missed ticks are materialized sequentially

### CRON materialization query

```sql
INSERT INTO executions (job_id, endpoint, endpoint_type, idempotency_key, status, input, run_at, max_attempts)
VALUES ($1, $2, $3, 'cron_' || $1 || '_' || extract(epoch from $4)::TEXT, 'QUEUED', $5, $4, $6)
ON CONFLICT (job_id, idempotency_key) DO NOTHING;
```

The `ON CONFLICT DO NOTHING` clause makes the insert idempotent — if the execution already exists (from a duplicate tick), the insert is silently skipped.

---

## Example: creating a job with idempotency key

### First request (new job)

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

### Duplicate request (same endpoint + idempotency key)

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

Response (`200 OK` — same job, no new execution):

```json
{
  "job_id": "job_8f3a...",
  "endpoint": "send-welcome-email",
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

:::note
The response is identical to the first request, except the HTTP status is `200 OK` instead of `201 Created`. No new execution is created — the existing one is returned.
:::

---

## Idempotency key best practices

1. **Use meaningful keys** — `order-1234-welcome-email` is better than a random UUID. Meaningful keys make debugging easier and are naturally unique per business event.

2. **Scope keys to the endpoint** — the unique constraint is on `(endpoint, idempotency_key)`, so the same key can be used across different endpoints. `order-1234-welcome` could be used for both `send-welcome-email` and `send-welcome-sms` endpoints.

3. **Include the business identifier** — derive the key from your domain: `{order_id}-{action}` or `{user_id}-{event_type}`.

4. **Don't reuse keys for different inputs** — if you change the input but reuse the idempotency key, you'll get the original job back, not a new one with the updated input.

:::warning
Idempotency keys are **not** a retry mechanism for updating jobs. If you need to fire the same endpoint with different input, use a different idempotency key. Reusing a key with different input will return the original job (with the original input) and silently ignore your new input.
:::

---

## What idempotency keys do and don't guarantee

Idempotency keys, the unique indexes, and `SELECT FOR UPDATE SKIP LOCKED` together guarantee that a job is **scheduled** once and claimed by one worker at a time:

| Mechanism | Purpose |
|-----------|---------|
| Idempotency keys | Prevent duplicate job creation |
| `idx_jobs_idempotency` | DB-level enforcement of job uniqueness |
| `idx_executions_cron_dedup` | DB-level enforcement of CRON tick uniqueness |
| `SELECT FOR UPDATE SKIP LOCKED` | Ensure only one worker claims an execution |
| `ON CONFLICT DO NOTHING` | CRON tick inserts are idempotent |
| CAS on `cron_next_run_at` | Prevent double-ticking when multiple schedulers run |

:::info
The `SELECT FOR UPDATE SKIP LOCKED` pattern ensures that when multiple workers poll for executions simultaneously, each execution is claimed by exactly one worker. Locked rows are skipped, so there's no contention or double-claiming.
:::

They do **not** guarantee that a target is called exactly once. The worker dispatches inside the transaction that holds the claim and commits afterwards, so a worker killed between the two redelivers on the next poll. Design receivers to tolerate that:

| Transport | How the key reaches the receiver |
|-----------|----------------------------------|
| `HTTP` | Automatic — sent as the `x-invokr-idempotency-key` header. Supply a header of that name in the endpoint spec to override it |
| `KAFKA` | **Not automatic.** Add it yourself: `"headers": { "idempotency-key": "{{execution.idempotency_key}}" }` |
| `REDIS_STREAM` | **Not automatic.** Add it yourself in `fields_template`: `{ "idempotency_key": "{{execution.idempotency_key}}" }` |


---

## See also

- [Jobs](./jobs) — where idempotency keys are provided
- [Executions](./executions) — CRON tick deduplication
- [Multi-Tenancy](./multi-tenancy) — how idempotency works across workspaces
- [The Three-Step Workflow](./overview) — where idempotency fits in the model
