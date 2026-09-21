---
id: exactly-once
title: Delivery Guarantees
---

# Delivery Guarantees

Invokr gives you **exactly-once scheduling** and **at-least-once delivery**.

Jobs are de-duplicated on the way in, each execution is claimed by one worker at a time, and nothing is silently dropped when a worker dies. What Invokr does *not* promise is that a target is called exactly once: the dispatch happens before the claim commits, so a worker lost mid-flight redelivers. Receivers are expected to be idempotent, and every HTTP dispatch carries the `x-invokr-idempotency-key` header so they can be.

| Stage | Guarantee | Mechanism |
|-------|-----------|-----------|
| Job creation | Exactly once per `(endpoint, idempotency_key)` | `idx_jobs_idempotency` unique partial index; a duplicate create returns the original job with `200 OK` |
| CRON tick | Exactly one execution per tick | `idx_executions_cron_dedup` unique partial index + `ON CONFLICT DO NOTHING` |
| Claiming | At most one worker at a time | `SELECT FOR UPDATE SKIP LOCKED` inside the scoped transaction |
| Delivery | **At least once** | The target is called inside the claiming transaction, before it commits |
| Outcome recording | Atomic | Attempt row, execution status and logs commit with the claim |
| Crash recovery | No lost executions | An uncommitted claim rolls back and is re-claimed by the next poll |

## Durability

Every job is persisted to PostgreSQL before the API acknowledges the request. The job and its initial execution are inserted in a single database transaction:

```sql
BEGIN;
INSERT INTO jobs (endpoint, endpoint_type, trigger_type, idempotency_key, input)
VALUES ($1, $2, 'IMMEDIATE', $3, $4)
RETURNING job_id;

INSERT INTO executions (job_id, endpoint, endpoint_type, idempotency_key, status, run_at, input, max_attempts)
VALUES ($5, $1, $2, $3, 'QUEUED', now(), $4, $6)
RETURNING execution_id, status, created_at;
COMMIT;
```

If the transaction commits, the job is durable. If the process crashes before the response is sent, the client can retry with the same idempotency key and get the original result. If the transaction rolls back, no partial state exists.

## Exactly-once scheduling

Scheduling is de-duplicated in three layers.

### 1. Idempotency keys + unique constraints

Every job creation carries an idempotency key — supplied by the client, or generated for you. The `idx_jobs_idempotency` unique partial index prevents duplicate job creation:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_idempotency
    ON jobs (endpoint, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

| Trigger Type | Key Provided By | Example |
|-------------|-----------------|---------|
| `IMMEDIATE` | Client, or a generated UUID when omitted | `order-1234-welcome-email` |
| `DELAYED` | Client (required) | `order-1234-reminder` |
| `CRON` | System, per tick | `cron_{job_id}_{epoch_ms}` |

For CRON ticks, the system generates the key as `cron_{job_id}_{epoch_ms}`, where `epoch_ms` is the current Unix timestamp in milliseconds. This ensures each tick produces a unique key, while the unique index prevents duplicate ticks within the same millisecond.

### 2. Execution-level deduplication

The `idx_executions_cron_dedup` unique partial index prevents duplicate executions for the same job + idempotency key combination:

```sql
CREATE UNIQUE INDEX IF NOT EXISTS idx_executions_cron_dedup
    ON executions (job_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

The CRON tick insert uses `ON CONFLICT DO NOTHING` to silently ignore duplicate ticks:

```sql
INSERT INTO executions (...)
VALUES (...)
ON CONFLICT (job_id, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING;
```

### 3. SKIP LOCKED for claiming

The worker claims executions using `SELECT FOR UPDATE SKIP LOCKED` within a transaction. Once a worker claims an execution, no other worker can claim it while that transaction is open:

- The row lock is held until the transaction commits or rolls back
- Other workers skip the locked row and try the next one
- The execution transitions from `QUEUED`/`PENDING`/`RETRYING` → `RUNNING` atomically within the claim

This gives you *at most one in-flight attempt per execution* — which is not the same thing as at most one delivery. See below.

## At-least-once delivery

The claim, the dispatch and the outcome all live in one scoped transaction, and the dispatch is a network call in the middle of it:

```
BEGIN (scoped to workspace schema)
  → Claim execution (SKIP LOCKED)         — status: QUEUED → RUNNING   (uncommitted)
  → Load endpoint
  → Load config (cached)
  → Load secrets (cached, decrypt)
  → Resolve templates
  → Dispatch to endpoint                  ← the target observes the job HERE
  → Record attempt
  → Finalize execution                    — status: RUNNING → SUCCESS / RETRYING / FAILED
COMMIT                                    ← the outcome becomes visible HERE
```

Between the dispatch and the commit there is a window. If the worker is killed — or loses its database connection — inside that window, the transaction rolls back: the execution returns to `QUEUED`/`PENDING`/`RETRYING` with its previous `attempt_count`, and the next worker to poll claims it and dispatches again. The target has then seen the same job twice, and Invokr has no record of the first delivery.

That ordering is what makes crash recovery free. There is no stuck-execution reaper and no lease expiry to wait out: a lost worker's claims are released by PostgreSQL itself, and the work is picked up on the next poll. The cost is that duplicate delivery is possible, so:

- **Make receivers idempotent.** Every HTTP dispatch sends the execution's idempotency key as the `x-invokr-idempotency-key` header (set a header of that name yourself to override it). De-duplicate on it.
- **Kafka and Redis Stream dispatches do not propagate the key automatically.** Template it into the message yourself with `{{execution.idempotency_key}}` — see [Templates](../core-concepts/templates).

:::info
The same window applies to a worker that is shut down gracefully, but only briefly: `SIGINT`/`SIGTERM` stops the poller and then waits up to `INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC` for in-flight transactions to commit, so a graceful stop normally drains rather than redelivers.
:::

## What Invokr does not guarantee

- **Exactly-once delivery at the target.** See above — design for at-least-once.
- **Ordering.** Executions are claimed oldest-`run_at`-first, but workers run up to `INVOKR_WORKER_MAX_CONCURRENT` of them in parallel across pods. Two jobs queued in order can complete out of order. If you need sequencing, sequence it in your own domain.
- **Fire-at-exact-time.** `run_at` is a floor, not a deadline: delayed executions are claimed on the next poll after `run_at` (default 200ms), and CRON ticks land at `pg_cron`'s one-minute granularity.

## Immutability

### CRON Job Immutability

CRON jobs are immutable. Updates create a new version and retire the old one, linked via `previous_version_id`:

```
job_abc (v1, ACTIVE)
    └──→ PUT /v1/jobs/job_abc
            └──→ job_def (v2, ACTIVE)  ← previous_version_id: job_abc
                     job_abc (v1, RETIRED)
```

The full version chain is preserved for audit. The `GET /v1/jobs/{job_id}/versions` endpoint returns the complete chain, newest to oldest.

One-shot jobs (`IMMEDIATE`, `DELAYED`) are also immutable — they fire once and complete. Attempting to update them returns `409 JOB_NOT_UPDATABLE`.

### Version Chain Schema

The `jobs` table tracks the version chain through two columns:

| Column | Type | Description |
|--------|------|-------------|
| `version` | `BIGINT` | Version number (starts at 1, incremented on update) |
| `previous_version_id` | `TEXT` | Job ID of the previous version (NULL for v1) |
| `replaced_by_id` | `TEXT` | Job ID of the new version (NULL if not replaced) |
| `status` | `TEXT` | `ACTIVE` or `RETIRED` (old version is retired when a new one is created) |

## Duplicate Request Handling

When a client sends a job creation request with an idempotency key that already exists, the API returns the existing entity with `200 OK` instead of `201 Created`:

| Scenario | HTTP Status | Response |
|----------|-------------|----------|
| New job created | `201 Created` | Full job + execution resource |
| Duplicate (same idempotency key) | `200 OK` | Existing job + execution resource |
| Duplicate CRON tick (same epoch_ms) | Silently ignored | `ON CONFLICT DO NOTHING` |

This allows clients to safely retry on network failures without fear of creating duplicate jobs.

## Transaction Boundaries

All execution state changes are atomic. If any step fails, the entire transaction rolls back — the execution stays in its previous state and is retried. If the transaction commits, all state changes (claim, attempt record, execution finalization, execution logs) are applied together.

:::info
The reaper also operates within this transaction boundary. When the reaper retires expired CRON jobs and unschedules their pg_cron entries, those changes commit atomically with the reaper execution's outcome. See [Reaper](./reaper) for details.
:::

## Related Pages

- [Database-Driven Scheduling](./db-driven-scheduling) — How pg_cron and SKIP LOCKED enable scheduling without a separate process
- [Worker Pipeline](./worker-pipeline) — The execution pipeline that processes claimed executions
- [Database Schema](./database-schema) — Full schema layout including all unique indexes
- [Idempotency](../core-concepts/idempotency) — Choosing and using idempotency keys
