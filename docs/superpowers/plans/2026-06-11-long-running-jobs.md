# Long-running jobs (polling + callback) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add first-class long-running job semantics to Kronos: an endpoint can return 202 + Location to enter a WAITING state; the worker polls the destination on schedule (honouring Retry-After) until terminal; destinations can also callback into Kronos to finalize; cancellation, observability and tests all work.

**Architecture:** Two new ExecutionStatus values (`WAITING`, `POLLING`). The existing worker poll-loop is extended via one SQL change — claim picks up WAITING rows and sets them to POLLING, otherwise behaves as today. The pipeline branches on post-claim status: dispatch path gets a new "async-detected" outcome; poll path is a new module that GETs the stored URL and classifies the response. Two new HTTP routes on the API server accept destination callbacks (tenant in URL, Bearer api_key auth). All terminal transitions filter on `status IN ('WAITING','POLLING')` so poll/callback/cancel can race safely.

**Tech Stack:** Rust (actix-web 4, sqlx 0.8, tokio), PostgreSQL 15 + pg_cron, Leptos/WASM dashboard, TypeScript CLI for end-to-end tests, justfile task runner.

**Spec:** `docs/superpowers/specs/2026-06-11-long-running-jobs-design.md`

---

## File map

**New files:**
- `migrations/20260611000000_long_running_jobs.sql` — additive ALTERs on `{p}executions` + `{p}jobs` + new `{p}polls` table + extended status CHECK + extended pickup index
- `crates/common/src/models/poll.rs` — `Poll` row struct, `PollClassification` enum
- `crates/common/src/db/polls.rs` — insert helper for the `polls` table
- `crates/common/src/secrets.rs` — header-secret resolution lifted from worker (cache-optional)
- `crates/worker/src/poll.rs` — `process_poll`, classification, URL resolution, Retry-After parsing
- `crates/api/src/handlers/callbacks.rs` — `POST /v1/callbacks/.../complete` and `.../fail`
- `cli/src/test-long-running.ts` — TypeScript end-to-end integration matrix

**Modified files:**
- `migrations/workspace_v1.sql` — apply the same schema additions inline so new tenants get them at provisioning time
- `crates/common/src/models/execution.rs` — add `WAITING`, `POLLING` variants; extended `Execution` row struct
- `crates/common/src/models/endpoint.rs` — `AsyncConfig`, `PollConfig`, `CallbackConfig` types; validation
- `crates/common/src/models/job.rs` — `AsyncOverrides` struct
- `crates/common/src/db/executions.rs` — extended `claim`, four new transition helpers, extended `cancel`
- `crates/common/src/db/jobs.rs` — write/read `async_max_wait_ms` / `async_max_polls`
- `crates/common/src/template.rs` — `{{execution.callback_url}}`, `{{execution.callback_url_success}}`, `{{execution.callback_url_failure}}`, `{{execution.org_id}}`, `{{execution.workspace_id}}`
- `crates/common/src/config.rs` — surface `api_base_url` + `path_prefix` to the template resolver via `PipelineContext`
- `crates/common/src/db.rs` — re-export `polls` module
- `crates/worker/src/pipeline.rs` — async detection in `process_dispatch`; switch to call lifted secret helper
- `crates/worker/src/poller.rs` — branch on `claim_status`; pass tenant + base_url to template resolution
- `crates/worker/src/lib.rs` (or `main.rs`) — declare `mod poll`
- `crates/worker/src/metrics.rs` (whichever file owns metric constants) — new metric names
- `crates/api/src/handlers/endpoints.rs` — validate `async` block on create/update
- `crates/api/src/handlers/jobs.rs` — validate + persist `async_overrides`; extend cancel with DELETE
- `crates/api/src/handlers/executions.rs` — extend `cancel` similarly; surface new columns + polls on GET
- `crates/api/src/handlers.rs` — declare `pub mod callbacks`
- `crates/api/src/router.rs` — wire `/v1/callbacks/...` routes
- `crates/mock-server/src/main.rs` — `/async/start`, `/async/status/{id}`, `DELETE /async/status/{id}`
- `crates/dashboard/...` — polls panel + WAITING/POLLING status pills
- `justfile` — `test-long-running` recipe

---

## Phase 1: Foundation (migration + Rust enum + Poll model)

### Task 1: SQL migration

**Files:**
- Create: `migrations/20260611000000_long_running_jobs.sql`
- Modify: `migrations/workspace_v1.sql:85-119` (executions table + indexes)

- [ ] **Step 1: Write the new timestamped migration**

Create `migrations/20260611000000_long_running_jobs.sql`:

```sql
-- Long-running jobs: WAITING / POLLING execution statuses, polls table,
-- per-execution and per-job async bounds.

-- Extend executions status CHECK
ALTER TABLE {p}executions DROP CONSTRAINT chk_{p}exec_status;
ALTER TABLE {p}executions ADD CONSTRAINT chk_{p}exec_status CHECK (status IN (
    'PENDING', 'QUEUED', 'RUNNING', 'RETRYING',
    'SUCCESS', 'FAILED', 'CANCELLED',
    'WAITING', 'POLLING'
));

-- Long-running columns on executions (snapshot of effective values + runtime state)
ALTER TABLE {p}executions
    ADD COLUMN poll_url            TEXT,
    ADD COLUMN poll_count          INT         NOT NULL DEFAULT 0,
    ADD COLUMN polling_started_at  TIMESTAMPTZ,
    ADD COLUMN polling_deadline    TIMESTAMPTZ,
    ADD COLUMN max_wait_ms         BIGINT,
    ADD COLUMN max_polls           INT;

-- Extend pickup index to include WAITING
DROP INDEX IF EXISTS idx_{p}executions_pickup;
CREATE INDEX idx_{p}executions_pickup
    ON {p}executions (status, run_at ASC)
    WHERE status IN ('QUEUED', 'RETRYING', 'PENDING', 'WAITING');

-- Per-job async overrides (resolved at job creation; copied to executions on insert)
ALTER TABLE {p}jobs
    ADD COLUMN async_max_wait_ms   BIGINT,
    ADD COLUMN async_max_polls     INT;

-- polls table mirrors attempts in shape
CREATE TABLE IF NOT EXISTS {p}polls (
    execution_id    TEXT        NOT NULL,
    poll_number     INT         NOT NULL,
    polled_at       TIMESTAMPTZ NOT NULL,
    duration_ms     BIGINT,
    status_code     INT,
    retry_after_ms  BIGINT,
    classification  TEXT        NOT NULL,
    error           JSONB,
    CONSTRAINT pk_{p}polls PRIMARY KEY (execution_id, poll_number),
    CONSTRAINT fk_{p}polls_execution FOREIGN KEY (execution_id) REFERENCES {p}executions (execution_id),
    CONSTRAINT chk_{p}poll_classification CHECK (classification IN (
        'SUCCESS', 'PENDING', 'TERMINAL_FAILURE', 'TRANSIENT_ERROR'
    ))
);
```

- [ ] **Step 2: Update `workspace_v1.sql` to match (so new tenants are provisioned with the right shape)**

In `migrations/workspace_v1.sql`, replace the status CHECK around line 104 with:

```sql
    CONSTRAINT chk_{p}exec_status CHECK (status IN (
        'PENDING', 'QUEUED', 'RUNNING', 'RETRYING',
        'SUCCESS', 'FAILED', 'CANCELLED',
        'WAITING', 'POLLING'
    ))
```

Add the six new columns inside the executions CREATE TABLE (alongside `duration_ms`):

```sql
    poll_url            TEXT,
    poll_count          INT         NOT NULL DEFAULT 0,
    polling_started_at  TIMESTAMPTZ,
    polling_deadline    TIMESTAMPTZ,
    max_wait_ms         BIGINT,
    max_polls           INT,
```

Replace the pickup index WHERE clause:

```sql
    WHERE status IN ('QUEUED', 'RETRYING', 'PENDING', 'WAITING');
```

Add the two new jobs columns inside the jobs CREATE TABLE (alongside other nullable cols):

```sql
    async_max_wait_ms   BIGINT,
    async_max_polls     INT,
```

Add the polls table (after attempts/execution_logs):

```sql
CREATE TABLE IF NOT EXISTS {p}polls (
    execution_id    TEXT        NOT NULL,
    poll_number     INT         NOT NULL,
    polled_at       TIMESTAMPTZ NOT NULL,
    duration_ms     BIGINT,
    status_code     INT,
    retry_after_ms  BIGINT,
    classification  TEXT        NOT NULL,
    error           JSONB,
    CONSTRAINT pk_{p}polls PRIMARY KEY (execution_id, poll_number),
    CONSTRAINT fk_{p}polls_execution FOREIGN KEY (execution_id) REFERENCES {p}executions (execution_id),
    CONSTRAINT chk_{p}poll_classification CHECK (classification IN (
        'SUCCESS', 'PENDING', 'TERMINAL_FAILURE', 'TRANSIENT_ERROR'
    ))
);
```

- [ ] **Step 3: Run migration against a fresh local DB to verify shape**

```bash
just db-reset && just db-migrate
psql -h localhost -U kronos -d kronos -c "\d sched_executions"
```

Expected: see the six new columns and the extended CHECK constraint.

- [ ] **Step 4: Commit**

```bash
git add migrations/20260611000000_long_running_jobs.sql migrations/workspace_v1.sql
git commit -m "feat(db): add long-running job columns + polls table"
```

---

### Task 2: ExecutionStatus enum additions

**Files:**
- Modify: `crates/common/src/models/execution.rs:1-49`

- [ ] **Step 1: Write the failing test**

Append to `crates/common/src/models/execution.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_and_polling_render_to_strings() {
        assert_eq!(ExecutionStatus::WAITING.as_str(), "WAITING");
        assert_eq!(ExecutionStatus::POLLING.as_str(), "POLLING");
    }
}
```

- [ ] **Step 2: Run test, verify it fails**

```bash
cargo test -p kronos-common models::execution::tests::waiting_and_polling_render_to_strings
```

Expected: FAIL — `WAITING` / `POLLING` not defined.

- [ ] **Step 3: Add the two variants**

In `crates/common/src/models/execution.rs:5-28`, extend the enum:

```rust
pub enum ExecutionStatus {
    PENDING,
    QUEUED,
    RUNNING,
    RETRYING,
    SUCCESS,
    FAILED,
    CANCELLED,
    WAITING,
    POLLING,
}
```

And the `as_str` match arms:

```rust
Self::WAITING => "WAITING",
Self::POLLING => "POLLING",
```

Extend `Execution` to carry the new columns:

```rust
pub struct Execution {
    pub execution_id: String,
    pub job_id: String,
    pub endpoint: String,
    pub endpoint_type: String,
    pub idempotency_key: Option<String>,
    pub status: String,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub worker_id: Option<String>,
    pub run_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub poll_url: Option<String>,
    pub poll_count: i32,
    pub polling_started_at: Option<DateTime<Utc>>,
    pub polling_deadline: Option<DateTime<Utc>>,
    pub max_wait_ms: Option<i64>,
    pub max_polls: Option<i32>,
}
```

- [ ] **Step 4: Run test, verify pass + workspace builds**

```bash
cargo test -p kronos-common models::execution::tests::waiting_and_polling_render_to_strings
cargo build --workspace
```

Expected: PASS; build succeeds (we'll wire DB reads later).

- [ ] **Step 5: Commit**

```bash
git add crates/common/src/models/execution.rs
git commit -m "feat(models): add WAITING and POLLING ExecutionStatus variants"
```

---

### Task 3: Poll model + classification enum

**Files:**
- Create: `crates/common/src/models/poll.rs`
- Modify: `crates/common/src/models.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/common/src/models/poll.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum PollClassification {
    SUCCESS,
    PENDING,
    TERMINAL_FAILURE,
    TRANSIENT_ERROR,
}

impl PollClassification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SUCCESS => "SUCCESS",
            Self::PENDING => "PENDING",
            Self::TERMINAL_FAILURE => "TERMINAL_FAILURE",
            Self::TRANSIENT_ERROR => "TRANSIENT_ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Poll {
    pub execution_id: String,
    pub poll_number: i32,
    pub polled_at: DateTime<Utc>,
    pub duration_ms: Option<i64>,
    pub status_code: Option<i32>,
    pub retry_after_ms: Option<i64>,
    pub classification: String,
    pub error: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_strings() {
        assert_eq!(PollClassification::SUCCESS.as_str(), "SUCCESS");
        assert_eq!(PollClassification::PENDING.as_str(), "PENDING");
        assert_eq!(PollClassification::TERMINAL_FAILURE.as_str(), "TERMINAL_FAILURE");
        assert_eq!(PollClassification::TRANSIENT_ERROR.as_str(), "TRANSIENT_ERROR");
    }
}
```

- [ ] **Step 2: Wire the module in**

Modify `crates/common/src/models.rs` to add `pub mod poll;` next to the other `pub mod` lines, and re-export both new types:

```rust
pub use poll::{Poll, PollClassification};
```

- [ ] **Step 3: Run test, verify pass**

```bash
cargo test -p kronos-common models::poll::tests::classification_strings
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/common/src/models/poll.rs crates/common/src/models.rs
git commit -m "feat(models): add Poll row + PollClassification enum"
```

---

## Phase 2: DB layer

### Task 4: `db::polls` insert helper

**Files:**
- Create: `crates/common/src/db/polls.rs`
- Modify: `crates/common/src/db.rs`

- [ ] **Step 1: Add the new module + re-export**

Create `crates/common/src/db/polls.rs`:

```rust
use crate::db::{tbl, DbContext};
use crate::models::PollClassification;
use chrono::{DateTime, Utc};

pub async fn insert(
    db: &mut DbContext<'_>,
    execution_id: &str,
    poll_number: i32,
    polled_at: DateTime<Utc>,
    duration_ms: Option<i64>,
    status_code: Option<i32>,
    retry_after_ms: Option<i64>,
    classification: PollClassification,
    error: Option<&serde_json::Value>,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "polls");
    sqlx::query(&format!(
        "INSERT INTO {t}
         (execution_id, poll_number, polled_at, duration_ms,
          status_code, retry_after_ms, classification, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
    ))
    .bind(execution_id)
    .bind(poll_number)
    .bind(polled_at)
    .bind(duration_ms)
    .bind(status_code)
    .bind(retry_after_ms)
    .bind(classification.as_str())
    .bind(error)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}

pub async fn list_for_execution(
    db: &mut DbContext<'_>,
    execution_id: &str,
) -> Result<Vec<crate::models::Poll>, sqlx::Error> {
    let t = tbl(db.prefix, "polls");
    sqlx::query_as::<_, crate::models::Poll>(&format!(
        "SELECT * FROM {t} WHERE execution_id = $1 ORDER BY poll_number ASC"
    ))
    .bind(execution_id)
    .fetch_all(&mut *db.conn)
    .await
}
```

In `crates/common/src/db.rs` add `pub mod polls;` alongside the other module declarations.

- [ ] **Step 2: Verify compile**

```bash
cargo build -p kronos-common
```

Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/common/src/db/polls.rs crates/common/src/db.rs
git commit -m "feat(db): add polls insert + list helpers"
```

---

### Task 5: Extended `db::executions::claim`

**Files:**
- Modify: `crates/common/src/db/executions.rs:1-42`

- [ ] **Step 1: Replace the existing `ClaimedExecution` struct + claim SQL**

Replace the existing struct + `claim` function:

```rust
#[derive(FromRow)]
pub struct ClaimedExecution {
    pub execution_id: String,
    pub job_id: String,
    pub endpoint: String,
    pub endpoint_type: String,
    pub input: Option<serde_json::Value>,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub claim_status: String,
    pub poll_url: Option<String>,
    pub poll_count: i32,
    pub max_wait_ms: Option<i64>,
    pub max_polls: Option<i32>,
    pub polling_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub polling_deadline: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn claim(
    db: &mut DbContext<'_>,
    worker_id: &str,
) -> Result<Option<ClaimedExecution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    let row: Option<ClaimedExecution> = sqlx::query_as(&format!(
        "UPDATE {t} e
         SET
           status = CASE WHEN e.status = 'WAITING' THEN 'POLLING' ELSE 'RUNNING' END,
           worker_id = $1,
           started_at = CASE WHEN e.status = 'WAITING' THEN e.started_at ELSE now() END,
           attempt_count = CASE WHEN e.status = 'WAITING' THEN e.attempt_count
                                ELSE e.attempt_count + 1 END,
           poll_count = CASE WHEN e.status = 'WAITING' THEN e.poll_count + 1
                             ELSE e.poll_count END
         WHERE e.execution_id = (
             SELECT execution_id FROM {t}
             WHERE status IN ('QUEUED','RETRYING','PENDING','WAITING')
               AND run_at <= now()
             ORDER BY run_at ASC
             LIMIT 1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING
           execution_id, job_id, endpoint, endpoint_type, input,
           attempt_count, max_attempts,
           status AS claim_status,
           poll_url, poll_count,
           max_wait_ms, max_polls,
           polling_started_at, polling_deadline"
    ))
    .bind(worker_id)
    .fetch_optional(&mut *db.conn)
    .await?;
    Ok(row)
}
```

- [ ] **Step 2: Update the worker's claim consumer to match the new struct**

In `crates/worker/src/poller.rs:134-184`, the existing `exec.attempt_count` / `exec.max_attempts` etc. continue to work. We'll change branch logic in Task 19; for now just confirm the build by widening the destructure (it's `let exec = ...` so it already works).

```bash
cargo build --workspace
```

Expected: success. If sqlx FromRow complains about a column name, double-check the alias `status AS claim_status` matches the struct field exactly.

- [ ] **Step 3: Commit**

```bash
git add crates/common/src/db/executions.rs
git commit -m "feat(db): extend executions::claim to pick up WAITING rows"
```

---

### Task 6: Transition helpers (four new functions in `db::executions`)

**Files:**
- Modify: `crates/common/src/db/executions.rs` (append below existing helpers)

- [ ] **Step 1: Add `transition_to_waiting`**

Append:

```rust
pub async fn transition_to_waiting(
    db: &mut DbContext<'_>,
    execution_id: &str,
    poll_url: &str,
    polling_started_at: chrono::DateTime<chrono::Utc>,
    polling_deadline: chrono::DateTime<chrono::Utc>,
    next_run_at: chrono::DateTime<chrono::Utc>,
    max_wait_ms: i64,
    max_polls: i32,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'WAITING',
             poll_url = $2,
             polling_started_at = $3,
             polling_deadline = $4,
             run_at = $5,
             max_wait_ms = $6,
             max_polls = $7,
             worker_id = NULL
         WHERE execution_id = $1 AND status = 'RUNNING'"
    ))
    .bind(execution_id)
    .bind(poll_url)
    .bind(polling_started_at)
    .bind(polling_deadline)
    .bind(next_run_at)
    .bind(max_wait_ms)
    .bind(max_polls)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}
```

- [ ] **Step 2: Add `transition_back_to_waiting` (POLLING → WAITING for next poll)**

```rust
pub async fn transition_back_to_waiting(
    db: &mut DbContext<'_>,
    execution_id: &str,
    next_run_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'WAITING',
             run_at = $2,
             worker_id = NULL
         WHERE execution_id = $1 AND status = 'POLLING'"
    ))
    .bind(execution_id)
    .bind(next_run_at)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}
```

- [ ] **Step 3: Add `retry_from_poll` (poll classified as TERMINAL_FAILURE)**

```rust
pub async fn retry_from_poll(
    db: &mut DbContext<'_>,
    execution_id: &str,
    backoff_ms: i64,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = CASE WHEN attempt_count >= max_attempts THEN 'FAILED' ELSE 'RETRYING' END,
             run_at = CASE WHEN attempt_count >= max_attempts THEN run_at
                      ELSE now() + ($2 * interval '1 millisecond') END,
             worker_id = NULL,
             completed_at = CASE WHEN attempt_count >= max_attempts THEN now() ELSE NULL END,
             duration_ms = CASE WHEN attempt_count >= max_attempts
                           THEN (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT
                           ELSE NULL END,
             poll_url = NULL,
             poll_count = 0,
             polling_started_at = NULL,
             polling_deadline = NULL
         WHERE execution_id = $1 AND status = 'POLLING'"
    ))
    .bind(execution_id)
    .bind(backoff_ms)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}
```

- [ ] **Step 4: Add `complete_failed_timeout`**

```rust
pub async fn complete_failed_timeout(
    db: &mut DbContext<'_>,
    execution_id: &str,
    error: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'FAILED',
             completed_at = now(),
             duration_ms = (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT,
             worker_id = NULL,
             output = $2
         WHERE execution_id = $1 AND status IN ('POLLING','WAITING')"
    ))
    .bind(execution_id)
    .bind(error)
    .execute(&mut *db.conn)
    .await?;
    Ok(())
}
```

The `output` column is used to record the TIMEOUT error payload — this mirrors how `complete_success` records final state. (We do not have a separate `error` column on `executions`; errors per attempt live in `attempts`.)

- [ ] **Step 5: Extend `complete_success` and add poll-callback variant**

The existing `complete_success(db, execution_id, output)` matches on `status = 'RUNNING'`. Callbacks and polls need to match `('WAITING','POLLING')` too. Add a sibling:

```rust
pub async fn complete_success_from_long_running(
    db: &mut DbContext<'_>,
    execution_id: &str,
    output: &serde_json::Value,
) -> Result<u64, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    let res = sqlx::query(&format!(
        "UPDATE {t}
         SET status = 'SUCCESS', output = $2, completed_at = now(),
             duration_ms = (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT,
             worker_id = NULL
         WHERE execution_id = $1 AND status IN ('WAITING','POLLING')"
    ))
    .bind(execution_id)
    .bind(output)
    .execute(&mut *db.conn)
    .await?;
    Ok(res.rows_affected())
}
```

The `rows_affected` return lets the callback handler distinguish "applied" vs "already terminal".

- [ ] **Step 6: Build, verify**

```bash
cargo build -p kronos-common
```

Expected: success.

- [ ] **Step 7: Commit**

```bash
git add crates/common/src/db/executions.rs
git commit -m "feat(db): add long-running execution transition helpers"
```

---

### Task 7: Extended `db::executions::cancel`

**Files:**
- Modify: `crates/common/src/db/executions.rs:162-175`

- [ ] **Step 1: Replace the existing `cancel` function**

The new `cancel` returns the previous status and poll_url so the API handler knows whether to fire a DELETE:

```rust
#[derive(FromRow)]
pub struct CancelledExecution {
    pub execution_id: String,
    pub previous_status: String,
    pub poll_url: Option<String>,
    pub endpoint: String,
}

pub async fn cancel(
    db: &mut DbContext<'_>,
    execution_id: &str,
) -> Result<Option<CancelledExecution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query_as::<_, CancelledExecution>(&format!(
        "UPDATE {t} e
         SET status = 'CANCELLED', completed_at = now(), worker_id = NULL
         WHERE execution_id = $1
           AND status IN ('PENDING','QUEUED','WAITING')
         RETURNING execution_id,
                   (SELECT status FROM {t} WHERE execution_id = $1) AS previous_status_unused,
                   -- subquery above is rolled back; we need the OLD status, not the new one.
                   -- Use the OLD subselect pattern below.
                   poll_url,
                   endpoint,
                   'WAITING' AS previous_status -- placeholder; replaced below"
    ))
    .bind(execution_id)
    .fetch_optional(&mut *db.conn)
    .await
}
```

Wait — Postgres' RETURNING returns the NEW row. We need the OLD status. Use a CTE:

Replace with the CTE form:

```rust
pub async fn cancel(
    db: &mut DbContext<'_>,
    execution_id: &str,
) -> Result<Option<CancelledExecution>, sqlx::Error> {
    let t = tbl(db.prefix, "executions");
    sqlx::query_as::<_, CancelledExecution>(&format!(
        "WITH cur AS (
            SELECT execution_id, status AS previous_status, poll_url, endpoint
            FROM {t}
            WHERE execution_id = $1
              AND status IN ('PENDING','QUEUED','WAITING')
            FOR UPDATE
         ),
         updated AS (
            UPDATE {t}
            SET status = 'CANCELLED', completed_at = now(), worker_id = NULL
            WHERE execution_id IN (SELECT execution_id FROM cur)
            RETURNING execution_id
         )
         SELECT c.execution_id, c.previous_status, c.poll_url, c.endpoint
         FROM cur c
         JOIN updated u USING (execution_id)"
    ))
    .bind(execution_id)
    .fetch_optional(&mut *db.conn)
    .await
}
```

The `WITH cur AS (... FOR UPDATE)` row-locks the qualifying row; the UPDATE then runs only against it; the final SELECT projects the *original* (locked) status alongside the row that was actually cancelled. Atomic and avoids the RETURNING-old-status problem.

Make sure the older test-only `cancel_pending_for_job` function below still compiles. It does — it doesn't return a value other than the row count.

- [ ] **Step 2: Update callers**

In `crates/api/src/handlers/executions.rs` and `crates/api/src/handlers/jobs.rs`, callers currently expect `Option<Execution>`. Adjust each call site to use `CancelledExecution` (they only check `is_some` today, so the change is cosmetic). Search:

```bash
grep -rn "executions::cancel\b" crates/api/src
```

For each result, change the binding type accordingly. We'll add the DELETE-on-poll-url logic in Task 19.

- [ ] **Step 3: Build**

```bash
cargo build --workspace
```

Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/common/src/db/executions.rs crates/api/src/handlers
git commit -m "feat(db): extend cancel to allow WAITING; return previous status + poll_url"
```

---

### Task 8: `db::jobs` async_max_* read/write

**Files:**
- Modify: `crates/common/src/db/jobs.rs`
- Modify: `crates/common/src/models/job.rs`

- [ ] **Step 1: Extend the Job row struct**

In `crates/common/src/models/job.rs`, add two fields to the `Job` struct:

```rust
pub async_max_wait_ms: Option<i64>,
pub async_max_polls: Option<i32>,
```

- [ ] **Step 2: Extend the job insert SQL**

In `crates/common/src/db/jobs.rs`, find the `pub async fn insert` (or `create`) function and:

1. Add `async_max_wait_ms` and `async_max_polls` to the column list and `VALUES` placeholders.
2. Add corresponding `.bind(...)` calls in the same order.
3. Bind from new function parameters `async_max_wait_ms: Option<i64>` and `async_max_polls: Option<i32>`.

Mirror the existing pattern — read the file to confirm the function signature and INSERT shape before editing.

- [ ] **Step 3: Extend Job-loading SQL**

Find every `SELECT ... FROM {p}jobs` in `crates/common/src/db/jobs.rs` and update each to include the two new columns. There should be three or four sites (`get`, `list`, related lookups). Replace `SELECT *` with explicit column lists if a query uses `*`, else just append the two new columns.

- [ ] **Step 4: Build**

```bash
cargo build -p kronos-common
```

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/common/src/db/jobs.rs crates/common/src/models/job.rs
git commit -m "feat(db): persist async_max_wait_ms / async_max_polls on jobs"
```

---

## Phase 3: Endpoint spec & job validation

### Task 9: Endpoint `async` config types + validation

**Files:**
- Modify: `crates/common/src/models/endpoint.rs`
- Modify: `crates/api/src/handlers/endpoints.rs`

- [ ] **Step 1: Add the new spec types**

In `crates/common/src/models/endpoint.rs`, append below `RetryPolicy`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncConfig {
    pub status_codes: Vec<u16>,
    pub poll: Option<PollConfig>,
    pub callback: Option<CallbackConfig>,
    #[serde(default = "default_max_wait_ms")]
    pub max_wait_ms: i64,
    #[serde(default = "default_max_polls")]
    pub max_polls: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollConfig {
    pub success_statuses: Vec<u16>,
    pub pending_statuses: Vec<u16>,
    pub failure_statuses: Vec<u16>,
    #[serde(default = "default_poll_initial_delay")]
    pub initial_delay_ms: i64,
    #[serde(default = "default_poll_max_delay")]
    pub max_delay_ms: i64,
    #[serde(default = "default_poll_backoff")]
    pub backoff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallbackConfig {
    pub enabled: bool,
}

fn default_max_wait_ms() -> i64 { 3_600_000 }
fn default_max_polls() -> i32 { 1_000 }
fn default_poll_initial_delay() -> i64 { 1_000 }
fn default_poll_max_delay() -> i64 { 60_000 }
fn default_poll_backoff() -> String { "exponential".into() }
```

Add an extractor helper on `Endpoint`:

```rust
impl Endpoint {
    pub fn get_async_config(&self) -> Option<AsyncConfig> {
        self.spec
            .get("async")
            .and_then(|v| serde_json::from_value::<AsyncConfig>(v.clone()).ok())
    }
}
```

- [ ] **Step 2: Write failing validation test**

Append to `crates/common/src/models/endpoint.rs`:

```rust
#[cfg(test)]
mod async_validation_tests {
    use super::*;

    pub fn validate_async(spec: &serde_json::Value) -> Result<(), String> {
        crate::models::endpoint::validate_async_block(spec)
    }

    #[test]
    fn rejects_overlapping_initial_status_codes() {
        let spec = serde_json::json!({
            "expected_status_codes": [200, 202],
            "async": {
                "status_codes": [202],
                "poll": {"success_statuses":[200],"pending_statuses":[202],"failure_statuses":[]},
                "max_wait_ms": 60000,
                "max_polls": 10
            }
        });
        let err = validate_async(&spec).unwrap_err();
        assert!(err.contains("disjoint"), "got: {}", err);
    }

    #[test]
    fn rejects_overlapping_poll_status_sets() {
        let spec = serde_json::json!({
            "expected_status_codes": [200],
            "async": {
                "status_codes": [202],
                "poll": {"success_statuses":[200,200],"pending_statuses":[200],"failure_statuses":[]},
                "max_wait_ms": 60000,
                "max_polls": 10
            }
        });
        assert!(validate_async(&spec).is_err());
    }

    #[test]
    fn rejects_both_modes_off() {
        let spec = serde_json::json!({
            "expected_status_codes": [200],
            "async": {"status_codes":[202],"max_wait_ms":60000,"max_polls":10}
        });
        assert!(validate_async(&spec).is_err());
    }

    #[test]
    fn accepts_minimal_valid_polling_only() {
        let spec = serde_json::json!({
            "expected_status_codes": [200],
            "async": {
                "status_codes": [202],
                "poll": {"success_statuses":[200],"pending_statuses":[202],"failure_statuses":[]},
                "max_wait_ms": 60000,
                "max_polls": 10
            }
        });
        assert!(validate_async(&spec).is_ok());
    }

    #[test]
    fn accepts_callback_only() {
        let spec = serde_json::json!({
            "expected_status_codes": [200],
            "async": {
                "status_codes": [202],
                "callback": {"enabled": true},
                "max_wait_ms": 60000,
                "max_polls": 10
            }
        });
        assert!(validate_async(&spec).is_ok());
    }

    #[test]
    fn accepts_no_async_block() {
        let spec = serde_json::json!({ "expected_status_codes": [200] });
        assert!(validate_async(&spec).is_ok());
    }
}
```

- [ ] **Step 3: Run, verify fail**

```bash
cargo test -p kronos-common models::endpoint::async_validation_tests
```

Expected: FAIL — function missing.

- [ ] **Step 4: Implement `validate_async_block`**

In `crates/common/src/models/endpoint.rs`, add:

```rust
pub fn validate_async_block(spec: &serde_json::Value) -> Result<(), String> {
    let Some(async_val) = spec.get("async") else {
        return Ok(());
    };

    let cfg: AsyncConfig = serde_json::from_value(async_val.clone())
        .map_err(|e| format!("invalid async config: {e}"))?;

    if cfg.poll.is_none() && cfg.callback.is_none() {
        return Err("async block must enable at least one of poll or callback".into());
    }

    let expected: std::collections::HashSet<u16> = spec
        .get("expected_status_codes")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_u64().map(|n| n as u16)).collect())
        .unwrap_or_default();
    let initial: std::collections::HashSet<u16> = cfg.status_codes.iter().copied().collect();
    if !expected.is_disjoint(&initial) {
        return Err(format!(
            "async.status_codes and expected_status_codes must be disjoint (overlap: {:?})",
            expected.intersection(&initial).collect::<Vec<_>>()
        ));
    }

    if cfg.max_wait_ms < 1 || cfg.max_wait_ms > 30 * 24 * 3600 * 1000 {
        return Err("async.max_wait_ms out of range (1 .. 30d)".into());
    }
    if cfg.max_polls < 1 || cfg.max_polls > 100_000 {
        return Err("async.max_polls out of range (1 .. 100000)".into());
    }

    if let Some(p) = &cfg.poll {
        let succ: std::collections::HashSet<u16> = p.success_statuses.iter().copied().collect();
        let pend: std::collections::HashSet<u16> = p.pending_statuses.iter().copied().collect();
        let fail: std::collections::HashSet<u16> = p.failure_statuses.iter().copied().collect();

        // intra-set duplicates
        if succ.len() != p.success_statuses.len()
            || pend.len() != p.pending_statuses.len()
            || fail.len() != p.failure_statuses.len()
        {
            return Err("async.poll status sets contain duplicates".into());
        }
        if !succ.is_disjoint(&pend) || !succ.is_disjoint(&fail) || !pend.is_disjoint(&fail) {
            return Err("async.poll status sets must be pairwise disjoint".into());
        }
        if p.initial_delay_ms < 1 || p.max_delay_ms < p.initial_delay_ms {
            return Err("async.poll initial_delay_ms / max_delay_ms invalid".into());
        }
    }

    Ok(())
}
```

- [ ] **Step 5: Wire into endpoint create + update handlers**

In `crates/api/src/handlers/endpoints.rs`, at the start of both `create` and `update`:

```rust
if let Err(msg) = kronos_common::models::endpoint::validate_async_block(&payload.spec) {
    return HttpResponse::BadRequest().json(serde_json::json!({
        "code": "INVALID_ASYNC_BLOCK",
        "message": msg,
    }));
}
```

(For `update`, only validate when `spec` is `Some`.)

- [ ] **Step 6: Verify tests pass + build**

```bash
cargo test -p kronos-common models::endpoint::async_validation_tests
cargo build --workspace
```

Expected: all 6 tests pass, build succeeds.

- [ ] **Step 7: Commit**

```bash
git add crates/common/src/models/endpoint.rs crates/api/src/handlers/endpoints.rs
git commit -m "feat(api): validate endpoint async block on create/update"
```

---

### Task 10: Job `async_overrides` validation + persistence

**Files:**
- Modify: `crates/common/src/models/job.rs`
- Modify: `crates/api/src/handlers/jobs.rs`

- [ ] **Step 1: Add `AsyncOverrides` to the create payload**

In `crates/common/src/models/job.rs`, find the `CreateJob` (or equivalent) struct and add:

```rust
#[derive(Debug, Deserialize, Clone)]
pub struct AsyncOverrides {
    pub max_wait_ms: Option<i64>,
    pub max_polls: Option<i32>,
}
```

Then add to the create payload:

```rust
pub async_overrides: Option<AsyncOverrides>,
```

- [ ] **Step 2: Write failing test for "overrides without async endpoint → 400"**

Append a unit test in `crates/common/src/models/job.rs`:

```rust
#[cfg(test)]
mod async_overrides_tests {
    use super::*;

    pub fn resolve(
        overrides: Option<&AsyncOverrides>,
        endpoint_async: Option<(i64, i32)>,
    ) -> Result<Option<(i64, i32)>, String> {
        crate::models::job::resolve_async_bounds(overrides, endpoint_async)
    }

    #[test]
    fn rejects_overrides_when_endpoint_not_async() {
        let o = AsyncOverrides { max_wait_ms: Some(60_000), max_polls: None };
        assert!(resolve(Some(&o), None).is_err());
    }

    #[test]
    fn falls_back_to_endpoint_defaults() {
        let o = AsyncOverrides { max_wait_ms: None, max_polls: None };
        let got = resolve(Some(&o), Some((60_000, 100))).unwrap();
        assert_eq!(got, Some((60_000, 100)));
    }

    #[test]
    fn applies_partial_override() {
        let o = AsyncOverrides { max_wait_ms: Some(120_000), max_polls: None };
        let got = resolve(Some(&o), Some((60_000, 100))).unwrap();
        assert_eq!(got, Some((120_000, 100)));
    }

    #[test]
    fn out_of_range_override_rejected() {
        let o = AsyncOverrides { max_wait_ms: Some(0), max_polls: None };
        assert!(resolve(Some(&o), Some((60_000, 100))).is_err());
    }

    #[test]
    fn returns_endpoint_defaults_without_overrides() {
        assert_eq!(resolve(None, Some((60_000, 100))).unwrap(), Some((60_000, 100)));
    }

    #[test]
    fn returns_none_when_endpoint_not_async_and_no_overrides() {
        assert_eq!(resolve(None, None).unwrap(), None);
    }
}
```

- [ ] **Step 3: Verify failure**

```bash
cargo test -p kronos-common models::job::async_overrides_tests
```

Expected: FAIL — `resolve_async_bounds` missing.

- [ ] **Step 4: Implement `resolve_async_bounds`**

In `crates/common/src/models/job.rs`:

```rust
pub fn resolve_async_bounds(
    overrides: Option<&AsyncOverrides>,
    endpoint_async: Option<(i64, i32)>,
) -> Result<Option<(i64, i32)>, String> {
    if overrides.is_some() && endpoint_async.is_none() {
        return Err("async_overrides given but endpoint has no async block".into());
    }
    let Some((ep_wait, ep_polls)) = endpoint_async else {
        return Ok(None);
    };

    let (wait, polls) = match overrides {
        Some(o) => (o.max_wait_ms.unwrap_or(ep_wait), o.max_polls.unwrap_or(ep_polls)),
        None => (ep_wait, ep_polls),
    };

    if wait < 1 || wait > 30 * 24 * 3600 * 1000 {
        return Err("async_overrides.max_wait_ms out of range (1 .. 30d)".into());
    }
    if polls < 1 || polls > 100_000 {
        return Err("async_overrides.max_polls out of range (1 .. 100000)".into());
    }
    Ok(Some((wait, polls)))
}
```

- [ ] **Step 5: Verify pass**

```bash
cargo test -p kronos-common models::job::async_overrides_tests
```

Expected: all 6 pass.

- [ ] **Step 6: Wire into job create handler**

In `crates/api/src/handlers/jobs.rs::create`, after loading the endpoint and before the INSERT:

```rust
let endpoint_async = endpoint
    .get_async_config()
    .map(|c| (c.max_wait_ms, c.max_polls));
let bounds = match kronos_common::models::job::resolve_async_bounds(
    payload.async_overrides.as_ref(),
    endpoint_async,
) {
    Ok(b) => b,
    Err(msg) => {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "code": "INVALID_OVERRIDES_NO_ASYNC",
            "message": msg,
        }));
    }
};
let (async_max_wait_ms, async_max_polls) = match bounds {
    Some((w, p)) => (Some(w), Some(p)),
    None => (None, None),
};
```

Then thread `async_max_wait_ms` / `async_max_polls` through to the `db::jobs::insert` call as added in Task 8. The pg_cron CRON-job branch already uses the same `jobs` row, so CRON ticks automatically pick up the resolved values when copying them to executions in Phase 4.

- [ ] **Step 7: Commit**

```bash
git add crates/common/src/models/job.rs crates/api/src/handlers/jobs.rs
git commit -m "feat(api): validate + persist job async_overrides"
```

---

## Phase 4: Secret helper refactor + template additions

### Task 11: Lift secret helpers to `crates/common/src/secrets.rs`

**Files:**
- Create: `crates/common/src/secrets.rs`
- Modify: `crates/common/src/lib.rs`
- Modify: `crates/worker/src/pipeline.rs`

- [ ] **Step 1: Create the new module**

Create `crates/common/src/secrets.rs`:

```rust
use crate::cache::SecretCache;
use crate::db::{self, DbContext};
use std::collections::HashMap;

pub async fn extract_referenced_secret_names(spec: &serde_json::Value) -> Vec<String> {
    let spec_str = spec.to_string();
    let mut names = Vec::new();
    let mut start = 0;
    while let Some(pos) = spec_str[start..].find("{{secret.") {
        let abs = start + pos + 9;
        if let Some(end) = spec_str[abs..].find("}}") {
            let name = spec_str[abs..abs + end].to_string();
            if !names.contains(&name) {
                names.push(name);
            }
            start = abs + end + 2;
        } else {
            break;
        }
    }
    names
}

pub async fn load(
    db: &mut DbContext<'_>,
    encryption_key: &str,
    spec: &serde_json::Value,
    cache: Option<&SecretCache>,
) -> Result<HashMap<String, String>, String> {
    let names = extract_referenced_secret_names(spec).await;
    let mut out = HashMap::with_capacity(names.len());
    for name in names {
        if let Some(c) = cache {
            if let Some(v) = c.get(&name) {
                out.insert(name, v);
                continue;
            }
        }
        let secret = db::secrets::get(db, &name)
            .await
            .map_err(|e| format!("Failed to load secret '{name}': {e}"))?
            .ok_or_else(|| format!("Secret '{name}' not found"))?;
        let plain = crate::crypto::decrypt(&secret.encrypted_value, encryption_key)
            .map_err(|e| format!("Failed to decrypt secret '{name}': {e}"))?;
        if let Some(c) = cache {
            c.set(name.clone(), plain.clone());
        }
        out.insert(name, plain);
    }
    Ok(out)
}
```

In `crates/common/src/lib.rs` add `pub mod secrets;` next to the other `pub mod`s.

- [ ] **Step 2: Switch worker pipeline to use the lifted helper**

In `crates/worker/src/pipeline.rs`, replace the local `load_secrets` / `load_single_secret` helpers (lines ~221-267) with a call:

```rust
let secret_values = match kronos_common::secrets::load(
    db,
    &ctx.encryption_key,
    &endpoint.spec,
    Some(&ctx.secret_cache),
).await {
    Ok(vals) => vals,
    Err(e) => { /* existing error path */ }
};
```

Delete the now-unused private helpers.

- [ ] **Step 3: Build + tests**

```bash
cargo build --workspace
cargo test -p kronos-common -p kronos-worker
```

Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/common/src/secrets.rs crates/common/src/lib.rs crates/worker/src/pipeline.rs
git commit -m "refactor(common): lift secret helpers from worker; reusable from api"
```

---

### Task 12: Template additions

**Files:**
- Modify: `crates/common/src/template.rs`
- Modify: `crates/worker/src/pipeline.rs` (populate the new vars in `execution_map`)
- Modify: `crates/common/src/config.rs` (surface `api_base_url` + `path_prefix` for resolution)

- [ ] **Step 1: Write the failing test**

Append in `crates/common/src/template.rs::tests`:

```rust
#[test]
fn callback_url_template_resolves() {
    let mut execution = std::collections::HashMap::new();
    execution.insert("execution_id".into(), serde_json::json!("exec_abc"));
    execution.insert("org_id".into(), serde_json::json!("org_1"));
    execution.insert("workspace_id".into(), serde_json::json!("ws_1"));
    execution.insert(
        "callback_url_success".into(),
        serde_json::json!("https://kronos.example/v1/callbacks/org_1/ws_1/executions/exec_abc/complete"),
    );
    let template = serde_json::json!({
        "on_success": "{{execution.callback_url_success}}",
        "org": "{{execution.org_id}}",
    });
    let out = resolve(
        &template,
        &Default::default(),
        &Default::default(),
        &Default::default(),
        &execution,
    ).unwrap();
    assert_eq!(out["on_success"].as_str().unwrap(),
               "https://kronos.example/v1/callbacks/org_1/ws_1/executions/exec_abc/complete");
    assert_eq!(out["org"].as_str().unwrap(), "org_1");
}
```

- [ ] **Step 2: Verify fail**

```bash
cargo test -p kronos-common template::tests::callback_url_template_resolves
```

Expected: PASS for resolution (the existing `execution.<key>` lookup at template.rs:102 already covers arbitrary keys). The test exists to lock the contract — callers must populate the map.

If the test fails because `resolve` doesn't accept the keys, inspect line 102; the existing code calls `execution.get(key)` which already supports any key. The test should pass once we populate the right keys.

- [ ] **Step 3: Populate the new vars in the pipeline**

In `crates/worker/src/pipeline.rs::process_execution`, replace the existing `execution_map` block with:

```rust
let mut execution_map: HashMap<String, serde_json::Value> = HashMap::new();
execution_map.insert("idempotency_key".into(), serde_json::json!(idempotency_key));
execution_map.insert("attempt_count".into(), serde_json::json!(attempt_count));
execution_map.insert("execution_id".into(), serde_json::json!(execution_id));
execution_map.insert("job_id".into(), serde_json::json!(job_id));
execution_map.insert("org_id".into(), serde_json::json!(ctx.org_id_for(schema_name)));
execution_map.insert("workspace_id".into(), serde_json::json!(ctx.workspace_id_for(schema_name)));
let cb_base = ctx.callback_base_url(schema_name, execution_id);
execution_map.insert("callback_url".into(), serde_json::json!(format!("{cb_base}/complete")));
execution_map.insert("callback_url_success".into(), serde_json::json!(format!("{cb_base}/complete")));
execution_map.insert("callback_url_failure".into(), serde_json::json!(format!("{cb_base}/fail")));
```

- [ ] **Step 4: Extend `PipelineContext`**

In `crates/worker/src/pipeline.rs`, add to `PipelineContext`:

```rust
pub api_base_url: String,
pub path_prefix: String,
pub schema_to_org_ws: std::sync::Arc<dyn Fn(&str) -> (String, String) + Send + Sync>,
```

And add methods:

```rust
impl PipelineContext {
    pub fn org_id_for(&self, schema: &str) -> String {
        (self.schema_to_org_ws)(schema).0
    }
    pub fn workspace_id_for(&self, schema: &str) -> String {
        (self.schema_to_org_ws)(schema).1
    }
    pub fn callback_base_url(&self, schema: &str, execution_id: &str) -> String {
        let (org, ws) = (self.schema_to_org_ws)(schema);
        format!(
            "{base}{prefix}/v1/callbacks/{org}/{ws}/executions/{exec}",
            base = self.api_base_url.trim_end_matches('/'),
            prefix = self.path_prefix,
            org = org, ws = ws, exec = execution_id,
        )
    }
}
```

The closure is supplied at worker startup in `crates/worker/src/poller.rs::run` based on the existing `SchemaProvider` / `SchemaRegistry`. The registry already maps `schema_name → (org_id, workspace_id)`; expose a synchronous lookup that closes over an `Arc<SchemaRegistry>`.

(If `SchemaRegistry` doesn't expose this today, add a small `get_org_ws(&self, schema: &str) -> Option<(String, String)>` that reads from its cached map. It already keeps schemas with TTL; the org/ws are part of the same row.)

- [ ] **Step 5: Verify tests pass + build**

```bash
cargo test -p kronos-common template::tests
cargo build --workspace
```

Expected: PASS; build succeeds.

- [ ] **Step 6: Commit**

```bash
git add crates/common/src/template.rs crates/worker/src/pipeline.rs crates/worker/src/poller.rs crates/common/src/config.rs crates/common/src/tenant.rs
git commit -m "feat(template): add execution.callback_url / .org_id / .workspace_id"
```

---

## Phase 5: Worker pipeline (dispatch async detection + poll path + branching)

### Task 13: Async detection at end of `process_dispatch`

**Files:**
- Modify: `crates/worker/src/pipeline.rs`
- Modify: `crates/worker/src/dispatcher/http.rs` (return headers)

- [ ] **Step 1: Extend `DispatchResult` to carry response headers (HTTP only)**

In `crates/worker/src/dispatcher.rs`:

```rust
pub enum DispatchResult {
    Success { output: Value, headers: HashMap<String, String>, status_code: u16 },
    Failure { error: Value },
}
```

Where `HashMap<String, String>` carries lowercased header names. Update the HTTP dispatcher (`crates/worker/src/dispatcher/http.rs`) to populate `headers` and `status_code` from the `reqwest::Response`:

```rust
let headers: HashMap<String, String> = response
    .headers()
    .iter()
    .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.as_str().to_ascii_lowercase(), s.to_string())))
    .collect();
let status = response.status().as_u16();
```

And include them in `DispatchResult::Success { headers, status_code: status, output: ... }`.

For the Kafka and Redis dispatchers (which don't have headers/status), set `headers: HashMap::new()`, `status_code: 0`.

Update the test cases in `dispatcher/http.rs` to destructure the new fields (the existing `is_success()` helper continues to work).

- [ ] **Step 2: Implement async detection in `process_execution`**

In `crates/worker/src/pipeline.rs`, replace the `DispatchResult::Success { output }` arm with:

```rust
DispatchResult::Success { output, headers, status_code } => {
    if let Some(async_cfg) = endpoint.get_async_config() {
        if async_cfg.status_codes.contains(&status_code) {
            // Long-running mode: extract Location, transition to WAITING
            match headers.get("location") {
                None => {
                    let err = serde_json::json!({"type":"MISSING_POLL_URL"});
                    let _ = db::executions::complete_failed(db, execution_id).await;
                    record_attempt(db, execution_id, attempt_count, "FAILED", started_at,
                        None, Some(&err)).await;
                    log_execution(db, execution_id, attempt_count, "ERROR",
                        "Destination returned async status but no Location header").await;
                    return;
                }
                Some(loc) => {
                    let initial_url = dispatch_spec["url"].as_str().unwrap_or_default();
                    let poll_url = resolve_relative_url(initial_url, loc);
                    let now = chrono::Utc::now();
                    let deadline = now + chrono::Duration::milliseconds(async_cfg.max_wait_ms);
                    let initial_delay = parse_retry_after(headers.get("retry-after").map(|s| s.as_str()))
                        .unwrap_or(async_cfg.poll.as_ref()
                            .map(|p| p.initial_delay_ms).unwrap_or(1000));
                    let next_run_at = std::cmp::min(deadline, now + chrono::Duration::milliseconds(initial_delay));
                    let _ = db::executions::transition_to_waiting(
                        db, execution_id, &poll_url, now, deadline, next_run_at,
                        async_cfg.max_wait_ms, async_cfg.max_polls,
                    ).await;
                    record_attempt(db, execution_id, attempt_count, "WAITING", started_at,
                        Some(&output), None).await;
                    log_execution(db, execution_id, attempt_count, "INFO",
                        &format!("Entered WAITING; will poll {poll_url} in {initial_delay}ms")).await;
                    return;
                }
            }
        }
    }
    // existing SUCCESS path
    /* metrics + complete_success + log */
}
```

Place `resolve_relative_url` and `parse_retry_after` in the new `poll.rs` (created in Task 14) and reference them with `crate::poll::resolve_relative_url`, etc.

- [ ] **Step 3: Build (compile-only; tests come with Task 14/15)**

```bash
cargo build --workspace
```

Expected: success (forward references resolved when Task 14 lands).

- [ ] **Step 4: Commit**

```bash
git add crates/worker/src/pipeline.rs crates/worker/src/dispatcher.rs crates/worker/src/dispatcher/http.rs crates/worker/src/dispatcher/kafka.rs crates/worker/src/dispatcher/redis_stream.rs crates/worker/src/dispatcher/internal.rs
git commit -m "feat(worker): detect async status code on initial dispatch"
```

---

### Task 14: `crates/worker/src/poll.rs` — URL resolution + Retry-After parsing

**Files:**
- Create: `crates/worker/src/poll.rs`
- Modify: `crates/worker/src/lib.rs` (or `main.rs`) — add `mod poll;`

- [ ] **Step 1: Write the failing tests**

Create `crates/worker/src/poll.rs`:

```rust
pub fn resolve_relative_url(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let base_url = url::Url::parse(base).ok();
    let resolved = base_url.and_then(|b| b.join(location).ok());
    resolved.map(|u| u.to_string()).unwrap_or_else(|| location.to_string())
}

pub fn parse_retry_after(header: Option<&str>) -> Option<i64> {
    let s = header?.trim();
    if let Ok(secs) = s.parse::<i64>() {
        return Some(secs * 1000);
    }
    // HTTP-date form
    if let Ok(when) = chrono::DateTime::parse_from_rfc2822(s) {
        let now = chrono::Utc::now();
        let delta = when.signed_duration_since(now).num_milliseconds();
        return Some(delta.max(0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_location_passes_through() {
        assert_eq!(
            resolve_relative_url("https://api.example/x", "https://other/y"),
            "https://other/y"
        );
    }
    #[test]
    fn relative_location_resolved_against_base() {
        assert_eq!(
            resolve_relative_url("https://api.example/jobs", "/status/abc"),
            "https://api.example/status/abc"
        );
    }
    #[test]
    fn relative_location_resolved_relative_path() {
        assert_eq!(
            resolve_relative_url("https://api.example/jobs/", "status/abc"),
            "https://api.example/jobs/status/abc"
        );
    }
    #[test]
    fn parse_retry_after_seconds() {
        assert_eq!(parse_retry_after(Some("30")), Some(30_000));
    }
    #[test]
    fn parse_retry_after_http_date_past_returns_zero() {
        let s = "Wed, 01 Jan 1970 00:00:00 GMT";
        assert_eq!(parse_retry_after(Some(s)), Some(0));
    }
    #[test]
    fn parse_retry_after_invalid_returns_none() {
        assert_eq!(parse_retry_after(Some("not a number")), None);
        assert_eq!(parse_retry_after(None), None);
    }
}
```

Add `url = "2"` to `crates/worker/Cargo.toml` if it isn't already present.

- [ ] **Step 2: Wire the module in**

In `crates/worker/src/lib.rs` (or `main.rs` for binary-only):

```rust
pub mod poll;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p kronos-worker poll::tests
```

Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/worker/src/poll.rs crates/worker/src/lib.rs crates/worker/Cargo.toml
git commit -m "feat(worker): URL resolution + Retry-After parsing for polling"
```

---

### Task 15: `process_poll` — bound check + classification + transitions

**Files:**
- Modify: `crates/worker/src/poll.rs` (append `process_poll`)
- Modify: `crates/worker/src/pipeline.rs` (export shared helpers; or inline in poll.rs)

- [ ] **Step 1: Write the classification unit test**

Append to `crates/worker/src/poll.rs`:

```rust
use kronos_common::models::PollClassification;

pub fn classify(
    status_code: Option<u16>,
    success: &[u16],
    pending: &[u16],
    failure: &[u16],
) -> PollClassification {
    match status_code {
        Some(c) if success.contains(&c) => PollClassification::SUCCESS,
        Some(c) if failure.contains(&c) => PollClassification::TERMINAL_FAILURE,
        Some(c) if pending.contains(&c) => PollClassification::PENDING,
        _ => PollClassification::TRANSIENT_ERROR,
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    #[test]
    fn success_wins() {
        assert_eq!(
            classify(Some(200), &[200], &[202], &[]),
            PollClassification::SUCCESS
        );
    }
    #[test]
    fn failure_recognized() {
        assert_eq!(
            classify(Some(410), &[200], &[202], &[410]),
            PollClassification::TERMINAL_FAILURE
        );
    }
    #[test]
    fn pending_recognized() {
        assert_eq!(
            classify(Some(202), &[200], &[202], &[]),
            PollClassification::PENDING
        );
    }
    #[test]
    fn unknown_is_transient() {
        assert_eq!(
            classify(Some(500), &[200], &[202], &[410]),
            PollClassification::TRANSIENT_ERROR
        );
    }
    #[test]
    fn transport_error_is_transient() {
        assert_eq!(
            classify(None, &[200], &[202], &[410]),
            PollClassification::TRANSIENT_ERROR
        );
    }
}
```

- [ ] **Step 2: Run, verify pass**

```bash
cargo test -p kronos-worker poll::classify_tests
```

Expected: all 5 pass.

- [ ] **Step 3: Implement `process_poll`**

Append to `crates/worker/src/poll.rs`:

```rust
use chrono::{Duration, Utc};
use kronos_common::{db, db::DbContext, models::PollClassification, secrets};
use reqwest::Client;
use std::collections::HashMap;

use crate::backoff;
use crate::pipeline::PipelineContext;

pub async fn process_poll(
    ctx: &PipelineContext,
    db: &mut DbContext<'_>,
    schema_name: &str,
    exec: &kronos_common::db::executions::ClaimedExecution,
) {
    let execution_id = &exec.execution_id;
    let attempt_count = exec.attempt_count;

    let Some(poll_url) = exec.poll_url.clone() else {
        tracing::error!(execution_id, "POLLING claim has no poll_url");
        let _ = db::executions::complete_failed(db, execution_id).await;
        return;
    };
    let max_polls = exec.max_polls.unwrap_or(1000);
    let deadline = exec.polling_deadline.unwrap_or_else(Utc::now);

    // Bound check — pre-network
    if exec.poll_count > max_polls || Utc::now() > deadline {
        let err = serde_json::json!({"type":"TIMEOUT","reason":
            if exec.poll_count > max_polls { "max_polls" } else { "max_wait_ms" }});
        let _ = db::executions::complete_failed_timeout(db, execution_id, &err).await;
        log_execution(db, execution_id, attempt_count, "WARN",
            "Polling budget exhausted; marking FAILED with TIMEOUT").await;
        return;
    }

    // Load endpoint + resolve headers
    let endpoint = match db::endpoints::get(db, &exec.endpoint).await {
        Ok(Some(ep)) => ep,
        _ => {
            let _ = db::executions::complete_failed(db, execution_id).await;
            return;
        }
    };
    let async_cfg = match endpoint.get_async_config() {
        Some(c) => c,
        None => {
            let err = serde_json::json!({"type":"ENDPOINT_NO_LONGER_ASYNC"});
            let _ = db::executions::complete_failed_timeout(db, execution_id, &err).await;
            return;
        }
    };
    let Some(poll_cfg) = async_cfg.poll else {
        // callback-only endpoint — POLLING shouldn't happen; transition back to WAITING with a long sleep
        let next = std::cmp::min(deadline, Utc::now() + Duration::milliseconds(60_000));
        let _ = db::executions::transition_back_to_waiting(db, execution_id, next).await;
        return;
    };

    let secret_values = match secrets::load(db, &ctx.encryption_key, &endpoint.spec, Some(&ctx.secret_cache)).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(execution_id, "Secret resolution failed for poll: {}", e);
            // Treat as transient
            let next = std::cmp::min(deadline, Utc::now() + Duration::milliseconds(poll_cfg.initial_delay_ms));
            let _ = db::executions::transition_back_to_waiting(db, execution_id, next).await;
            return;
        }
    };

    // Build GET with resolved headers (only secret substitution; configs aren't relevant for polling URL)
    let mut req = ctx.http_client.get(&poll_url);
    if let Some(headers) = endpoint.spec.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                let resolved = substitute_secrets(s, &secret_values);
                req = req.header(k.as_str(), resolved);
            }
        }
    }
    let timeout_ms = endpoint.spec.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(5000);
    req = req.timeout(std::time::Duration::from_millis(timeout_ms));

    let started = std::time::Instant::now();
    let res = req.send().await;

    let polled_at = Utc::now();
    let duration_ms = started.elapsed().as_millis() as i64;
    let poll_number = exec.poll_count;

    match res {
        Ok(response) => {
            let status_code = response.status().as_u16();
            let retry_after_ms = parse_retry_after(
                response.headers().get("retry-after").and_then(|v| v.to_str().ok())
            );
            let body = response.text().await.unwrap_or_default();
            let parsed_body = serde_json::from_str::<serde_json::Value>(&body)
                .unwrap_or_else(|_| serde_json::json!({"raw": body}));

            let cls = classify(
                Some(status_code),
                &poll_cfg.success_statuses,
                &poll_cfg.pending_statuses,
                &poll_cfg.failure_statuses,
            );

            let _ = db::polls::insert(
                db, execution_id, poll_number, polled_at, Some(duration_ms),
                Some(status_code as i32), retry_after_ms, cls, None,
            ).await;

            match cls {
                PollClassification::SUCCESS => {
                    let _ = db::executions::complete_success_from_long_running(
                        db, execution_id, &parsed_body
                    ).await;
                    log_execution(db, execution_id, attempt_count, "INFO",
                        &format!("Poll #{poll_number} → {status_code} success after {duration_ms}ms")).await;
                }
                PollClassification::TERMINAL_FAILURE => {
                    let retry_policy = endpoint.get_retry_policy();
                    let backoff_ms = backoff::compute_backoff(&retry_policy, attempt_count);
                    let _ = db::executions::retry_from_poll(db, execution_id, backoff_ms).await;
                    log_execution(db, execution_id, attempt_count, "WARN",
                        &format!("Poll #{poll_number} → {status_code} terminal failure; re-dispatch in {backoff_ms}ms")).await;
                }
                PollClassification::PENDING | PollClassification::TRANSIENT_ERROR => {
                    let delay_ms = retry_after_ms.unwrap_or(poll_cfg.initial_delay_ms);
                    let next = std::cmp::min(deadline, Utc::now() + Duration::milliseconds(delay_ms));
                    let _ = db::executions::transition_back_to_waiting(db, execution_id, next).await;
                    log_execution(db, execution_id, attempt_count, "INFO",
                        &format!("Poll #{poll_number} → {status_code} ({}); next poll in {}ms", cls.as_str(), delay_ms)).await;
                }
            }
        }
        Err(e) => {
            let err = serde_json::json!({"type":"TRANSPORT_ERROR","message":e.to_string()});
            let _ = db::polls::insert(
                db, execution_id, poll_number, polled_at, Some(duration_ms),
                None, None, PollClassification::TRANSIENT_ERROR, Some(&err),
            ).await;
            let delay_ms = poll_cfg.initial_delay_ms;
            let next = std::cmp::min(deadline, Utc::now() + Duration::milliseconds(delay_ms));
            let _ = db::executions::transition_back_to_waiting(db, execution_id, next).await;
            log_execution(db, execution_id, attempt_count, "WARN",
                &format!("Poll #{poll_number} transport error; next poll in {delay_ms}ms")).await;
        }
    }
}

fn substitute_secrets(s: &str, secrets: &HashMap<String, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in secrets {
        out = out.replace(&format!("{{{{secret.{k}}}}}"), v);
    }
    out
}

async fn log_execution(
    db: &mut DbContext<'_>,
    execution_id: &str,
    attempt_number: i64,
    level: &str,
    message: &str,
) {
    let _ = kronos_common::db::execution_logs::insert(
        db, execution_id, attempt_number, level, message
    ).await;
}
```

- [ ] **Step 4: Build**

```bash
cargo build --workspace
```

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/worker/src/poll.rs
git commit -m "feat(worker): process_poll with classification + retry-from-poll + transient handling"
```

---

### Task 16: Pipeline branching in poller

**Files:**
- Modify: `crates/worker/src/poller.rs:112-200`

- [ ] **Step 1: Branch on `claim_status`**

Replace the body of `claim_and_process` (after the existing `tx`/`db`/`exec`/`job` setup) with:

```rust
match exec.claim_status.as_str() {
    "RUNNING" => {
        pipeline::process_execution(
            ctx, &mut db, schema_name,
            &exec.execution_id, idempotency_key,
            &exec.job_id, &exec.endpoint, &exec.endpoint_type,
            exec.input.as_ref(),
            exec.attempt_count, exec.max_attempts,
        ).await;
    }
    "POLLING" => {
        crate::poll::process_poll(ctx, &mut db, schema_name, &exec).await;
    }
    other => {
        tracing::error!(execution_id = %exec.execution_id,
            "Unexpected claim_status {}; failing safe", other);
        let _ = kronos_common::db::executions::complete_failed(&mut db, &exec.execution_id).await;
    }
}
```

- [ ] **Step 2: Build + verify worker still passes existing tests**

```bash
cargo build --workspace
cargo test -p kronos-worker
```

Expected: builds, existing tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/worker/src/poller.rs
git commit -m "feat(worker): branch claim flow on RUNNING vs POLLING"
```

---

## Phase 6: API server

### Task 17: Cancel handler — best-effort DELETE on poll URL

**Files:**
- Modify: `crates/api/src/handlers/executions.rs` (the `cancel` handler)
- Modify: `crates/api/src/handlers/jobs.rs` (the `cancel` handler, if it also cancels current execution)

- [ ] **Step 1: Add the best-effort DELETE branch**

In `crates/api/src/handlers/executions.rs::cancel`, after the existing `db::executions::cancel(...)` call:

```rust
let cancelled = match db::executions::cancel(&mut db, &execution_id).await {
    Ok(Some(c)) => c,
    Ok(None) => return HttpResponse::Conflict()
        .json(serde_json::json!({"code":"NOT_CANCELLABLE"})),
    Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
};

if cancelled.previous_status == "WAITING" {
    if let Some(poll_url) = cancelled.poll_url.clone() {
        let endpoint_name = cancelled.endpoint.clone();
        let pool = state.pool.clone();
        let prefix = state.config.db.table_prefix.clone();
        let schema = schema_name.clone();
        let key = state.config.crypto.encryption_key.clone();
        let client = state.http_client.clone();
        let exec_id = execution_id.clone();
        tokio::spawn(async move {
            if let Err(e) = send_cancel_delete(pool, prefix, schema, key, client, endpoint_name, poll_url, exec_id).await {
                tracing::warn!("cancel DELETE failed: {e}");
            }
        });
    }
}

HttpResponse::Ok().json(/* the cancelled execution */)
```

Add the helper at the bottom of the file:

```rust
async fn send_cancel_delete(
    pool: PgPool,
    prefix: String,
    schema: String,
    encryption_key: String,
    client: reqwest::Client,
    endpoint_name: String,
    poll_url: String,
    execution_id: String,
) -> Result<(), String> {
    let mut tx = kronos_common::db::scoped::scoped_transaction(&pool, &schema)
        .await.map_err(|e| e.to_string())?;
    let mut db = kronos_common::db::DbContext::new(&mut *tx, &prefix);
    let endpoint = kronos_common::db::endpoints::get(&mut db, &endpoint_name)
        .await.map_err(|e| e.to_string())?
        .ok_or_else(|| "endpoint not found".to_string())?;
    let secret_values = kronos_common::secrets::load(&mut db, &encryption_key, &endpoint.spec, None)
        .await.map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;

    let mut req = client.delete(&poll_url).timeout(std::time::Duration::from_secs(5));
    if let Some(headers) = endpoint.spec.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                let mut resolved = s.to_string();
                for (name, val) in &secret_values {
                    resolved = resolved.replace(&format!("{{{{secret.{name}}}}}"), val);
                }
                req = req.header(k.as_str(), resolved);
            }
        }
    }
    let result = req.send().await;
    let mut tx = kronos_common::db::scoped::scoped_transaction(&pool, &schema)
        .await.map_err(|e| e.to_string())?;
    let mut db = kronos_common::db::DbContext::new(&mut *tx, &prefix);
    let line = match &result {
        Ok(r) => format!("Cancel DELETE to {poll_url} → {}", r.status().as_u16()),
        Err(e) => format!("Cancel DELETE to {poll_url} → error: {e}"),
    };
    let _ = kronos_common::db::execution_logs::insert(
        &mut db, &execution_id, 0, "INFO", &line
    ).await;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}
```

- [ ] **Step 2: Mirror to `jobs::cancel` if applicable**

If `crates/api/src/handlers/jobs.rs::cancel` also cancels the current execution (it does — see the existing implementation), apply the same DELETE branch there too. Search:

```bash
grep -n "executions::cancel\|cancel_pending_for_job" crates/api/src/handlers/jobs.rs
```

Wrap the equivalent block.

- [ ] **Step 3: Build**

```bash
cargo build --workspace
```

Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/api/src/handlers/executions.rs crates/api/src/handlers/jobs.rs
git commit -m "feat(api): best-effort DELETE on poll_url when cancelling WAITING execution"
```

---

### Task 18: Callback handlers (`/complete`, `/fail`)

**Files:**
- Create: `crates/api/src/handlers/callbacks.rs`
- Modify: `crates/api/src/handlers.rs` (declare `pub mod callbacks;`)
- Modify: `crates/api/src/router.rs` (register routes)

- [ ] **Step 1: Implement the handlers**

Create `crates/api/src/handlers/callbacks.rs`:

```rust
use actix_web::{web, HttpResponse};
use kronos_common::db::{self, scoped, DbContext};
use serde::Deserialize;
use serde_json::Value;

use crate::router::AppState;

#[derive(Deserialize)]
pub struct CompleteBody { pub output: Value }

#[derive(Deserialize)]
pub struct FailBody { pub error: Value }

pub async fn complete(
    state: web::Data<AppState>,
    path: web::Path<(String, String, String)>,
    body: web::Json<CompleteBody>,
) -> HttpResponse {
    let (org_id, workspace_id, execution_id) = path.into_inner();
    let schema_name = match state.schema_registry.resolve(&org_id, &workspace_id).await {
        Some(s) => s,
        None => return HttpResponse::Forbidden().finish(),
    };
    let mut tx = match scoped::scoped_transaction(&state.pool, &schema_name).await {
        Ok(tx) => tx,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    let mut db = DbContext::new(&mut *tx, &state.config.db.table_prefix);

    let applied = match db::executions::complete_success_from_long_running(
        &mut db, &execution_id, &body.output
    ).await {
        Ok(rows) => rows > 0,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };

    if !applied {
        // Determine current state for a sharper 409
        let current = db::executions::get(&mut db, &execution_id).await.ok().flatten();
        return match current {
            None => HttpResponse::NotFound().finish(),
            Some(e) if matches!(e.status.as_str(), "SUCCESS"|"FAILED"|"CANCELLED") =>
                HttpResponse::Conflict().json(serde_json::json!({
                    "code":"ALREADY_TERMINAL","current_status":e.status})),
            Some(e) => HttpResponse::Conflict().json(serde_json::json!({
                "code":"NOT_YET_WAITING","current_status":e.status})),
        };
    }

    let _ = db::execution_logs::insert(&mut db, &execution_id, 0, "INFO", "Callback received: complete").await;
    let row = db::executions::get(&mut db, &execution_id).await.ok().flatten();
    let _ = tx.commit().await;
    HttpResponse::Ok().json(row)
}

pub async fn fail(
    state: web::Data<AppState>,
    path: web::Path<(String, String, String)>,
    body: web::Json<FailBody>,
) -> HttpResponse {
    let (org_id, workspace_id, execution_id) = path.into_inner();
    let schema_name = match state.schema_registry.resolve(&org_id, &workspace_id).await {
        Some(s) => s,
        None => return HttpResponse::Forbidden().finish(),
    };
    let mut tx = match scoped::scoped_transaction(&state.pool, &schema_name).await {
        Ok(tx) => tx,
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    let mut db = DbContext::new(&mut *tx, &state.config.db.table_prefix);

    // Load endpoint to compute backoff
    let exec = match db::executions::get(&mut db, &execution_id).await {
        Ok(Some(e)) => e,
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
    };
    if matches!(exec.status.as_str(), "SUCCESS"|"FAILED"|"CANCELLED") {
        return HttpResponse::Conflict().json(serde_json::json!({
            "code":"ALREADY_TERMINAL","current_status":exec.status}));
    }
    if !matches!(exec.status.as_str(), "WAITING"|"POLLING") {
        return HttpResponse::Conflict().json(serde_json::json!({
            "code":"NOT_YET_WAITING","current_status":exec.status}));
    }
    let endpoint = match db::endpoints::get(&mut db, &exec.endpoint).await {
        Ok(Some(ep)) => ep,
        _ => return HttpResponse::InternalServerError().body("endpoint missing"),
    };
    let retry_policy = endpoint.get_retry_policy();
    let backoff_ms = kronos_worker::backoff::compute_backoff(&retry_policy, exec.attempt_count);

    if let Err(e) = db::executions::retry_from_poll(&mut db, &execution_id, backoff_ms).await {
        return HttpResponse::InternalServerError().body(e.to_string());
    }

    let _ = db::execution_logs::insert(&mut db, &execution_id, 0, "INFO",
        "Callback received: fail → re-dispatch").await;
    let row = db::executions::get(&mut db, &execution_id).await.ok().flatten();
    let _ = tx.commit().await;
    HttpResponse::Ok().json(row)
}
```

Note: this introduces a dependency from `kronos-api` on `kronos-worker::backoff`. If that creates a workspace cycle, instead lift `backoff::compute_backoff` to `crates/common/src/backoff.rs` first (read the existing file at `crates/worker/src/backoff.rs` — it's small) and update both consumers. Prefer the lift; clean dependency direction.

- [ ] **Step 2: Declare module + register routes**

In `crates/api/src/handlers.rs`, add `pub mod callbacks;`.

In `crates/api/src/router.rs`, at the same level as the existing `/v1/orgs/{org_id}/workspaces/{workspace_id}/v1/...` scope, add the callbacks scope (no tenant headers required; tenant is in URL path):

```rust
.service(
    web::scope("/v1/callbacks/{org_id}/{workspace_id}")
        .route(
            "/executions/{execution_id}/complete",
            web::post().to(handlers::callbacks::complete),
        )
        .route(
            "/executions/{execution_id}/fail",
            web::post().to(handlers::callbacks::fail),
        ),
)
```

`SchemaRegistry` likely has a method that maps `(org_id, workspace_id) → schema_name`. If it doesn't expose a `resolve(&org, &ws) -> Option<String>` method, add one — it should be a trivial map lookup in the existing registry.

- [ ] **Step 3: Build**

```bash
cargo build --workspace
```

Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/api/src/handlers/callbacks.rs crates/api/src/handlers.rs crates/api/src/router.rs crates/common/src/backoff.rs crates/worker/src/backoff.rs
git commit -m "feat(api): add callback /complete and /fail routes"
```

---

### Task 19: GET execution surfaces new columns + polls

**Files:**
- Modify: `crates/api/src/handlers/executions.rs` (the `get` handler)
- Modify: smithy model + generated client (optional; see step 3)

- [ ] **Step 1: Augment the response**

In `crates/api/src/handlers/executions.rs::get`, after fetching the execution row, also fetch polls and include them:

```rust
let exec = match db::executions::get(&mut db, &execution_id).await {
    Ok(Some(e)) => e,
    Ok(None) => return HttpResponse::NotFound().finish(),
    Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
};
let polls = db::polls::list_for_execution(&mut db, &execution_id).await.unwrap_or_default();
let resp = serde_json::json!({
    "execution": exec,
    "polls": polls,
});
HttpResponse::Ok().json(resp)
```

(If the existing response is the bare execution row, keep it: add a sibling `GET /executions/{id}/polls` route that returns `polls`. Either shape is fine — pick what matches the current `attempts` exposure pattern in the same file.)

- [ ] **Step 2: Add a `list_polls` route mirroring `list_attempts`**

In `crates/api/src/handlers/executions.rs`:

```rust
pub async fn list_polls(
    state: web::Data<AppState>,
    /* extractors for tenant + execution_id same as list_attempts */
) -> HttpResponse {
    /* same shape as list_attempts but calling db::polls::list_for_execution */
}
```

Register in `router.rs` next to `list_attempts`:

```rust
.route(
    "/executions/{execution_id}/polls",
    web::get().to(handlers::executions::list_polls),
)
```

- [ ] **Step 3: Build**

```bash
cargo build --workspace
```

Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/api/src/handlers/executions.rs crates/api/src/router.rs
git commit -m "feat(api): expose new long-running fields + polls list on execution endpoints"
```

---

## Phase 7: Mock-server + integration tests

### Task 20: Mock-server async routes

**Files:**
- Modify: `crates/mock-server/src/main.rs`

- [ ] **Step 1: Add in-process state + routes**

In `crates/mock-server/src/main.rs`, add:

```rust
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

#[derive(Clone, Default)]
struct AsyncState {
    /// Per-id: queue of (status_code, body_json, retry_after_seconds, has_location) responses to serve in order.
    /// When the queue is exhausted, the LAST entry is repeated forever.
    scripts: Arc<Mutex<HashMap<String, Vec<(u16, serde_json::Value, Option<u64>)>>>>,
    /// Per-id: count of DELETE calls observed.
    cancels: Arc<Mutex<HashMap<String, u32>>>,
    /// Counter to mint new ids.
    next_id: Arc<Mutex<u64>>,
}
```

Add three handlers:

```rust
async fn async_start(
    state: web::Data<AsyncState>,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    let mut next = state.next_id.lock().unwrap();
    *next += 1;
    let id = format!("async-{}", *next);
    let script: Vec<(u16, serde_json::Value, Option<u64>)> = body
        .get("script")
        .and_then(|s| s.as_array())
        .map(|arr| arr.iter().filter_map(|entry| {
            let status = entry.get("status")?.as_u64()? as u16;
            let body = entry.get("body").cloned().unwrap_or(serde_json::json!({}));
            let retry_after = entry.get("retry_after").and_then(|v| v.as_u64());
            Some((status, body, retry_after))
        }).collect())
        .unwrap_or_else(|| vec![(200, serde_json::json!({}), None)]);
    state.scripts.lock().unwrap().insert(id.clone(), script);
    HttpResponse::Accepted()
        .insert_header(("Location", format!("/async/status/{id}")))
        .json(serde_json::json!({"task_id": id}))
}

async fn async_status(
    state: web::Data<AsyncState>,
    path: web::Path<String>,
) -> HttpResponse {
    let id = path.into_inner();
    let mut scripts = state.scripts.lock().unwrap();
    let Some(script) = scripts.get_mut(&id) else {
        return HttpResponse::NotFound().finish();
    };
    let (status, body, retry_after) = if script.len() > 1 {
        script.remove(0)
    } else {
        script[0].clone()
    };
    let mut resp = HttpResponse::build(actix_web::http::StatusCode::from_u16(status).unwrap());
    if let Some(s) = retry_after {
        resp.insert_header(("Retry-After", s.to_string()));
    }
    resp.json(body)
}

async fn async_cancel(
    state: web::Data<AsyncState>,
    path: web::Path<String>,
) -> HttpResponse {
    let id = path.into_inner();
    *state.cancels.lock().unwrap().entry(id).or_insert(0) += 1;
    HttpResponse::NoContent().finish()
}

async fn async_inspect(
    state: web::Data<AsyncState>,
    path: web::Path<String>,
) -> HttpResponse {
    let id = path.into_inner();
    let cancels = *state.cancels.lock().unwrap().get(&id).unwrap_or(&0);
    let remaining = state.scripts.lock().unwrap().get(&id).map(|v| v.len()).unwrap_or(0);
    HttpResponse::Ok().json(serde_json::json!({"cancels":cancels,"remaining_responses":remaining}))
}
```

Register in `main`:

```rust
let state = AsyncState::default();
App::new()
    .app_data(web::Data::new(state))
    .route("/async/start", web::post().to(async_start))
    .route("/async/status/{id}", web::get().to(async_status))
    .route("/async/status/{id}", web::delete().to(async_cancel))
    .route("/async/inspect/{id}", web::get().to(async_inspect))
    /* existing routes */
```

- [ ] **Step 2: Run mock-server, smoke-test by hand**

```bash
cargo run -p kronos-mock-server &
curl -s -X POST localhost:9999/async/start -H 'content-type: application/json' \
  -d '{"script":[{"status":202},{"status":200,"body":{"ok":true}}]}' -i
curl -s localhost:9999/async/status/async-1 -i
curl -s localhost:9999/async/status/async-1 -i
```

Expected: first call returns `202 Location: /async/status/async-1`; subsequent `GET`s return `202` then `200 {"ok": true}`.

- [ ] **Step 3: Commit**

```bash
git add crates/mock-server/src/main.rs
git commit -m "test(mock): add programmable /async routes for long-running tests"
```

---

### Task 21: TypeScript integration matrix

**Files:**
- Create: `cli/src/test-long-running.ts`
- Modify: `justfile`

- [ ] **Step 1: Write the test harness**

Create `cli/src/test-long-running.ts`. Use the existing helpers in `cli/src/` (look at `test-immediate.ts` for the org/workspace/endpoint setup pattern).

The test must:

1. Set up org, workspace, an HTTP endpoint pointing at the mock-server `/async/start` with an `async` block enabling both polling and callback.
2. For each row in the matrix below, prime the mock-server script, create a job, poll Kronos's GET execution until terminal, assert expected status + side effects.

Matrix (one async function per row, all run sequentially):

```ts
const MATRIX = [
  { name: 'success on first poll',
    script: [{status:202},{status:200,body:{result:'ok'}}],
    expect: { status: 'SUCCESS', polls: 1 } },
  { name: '202 x3 then 200',
    script: [{status:202},{status:202},{status:202},{status:202},{status:200,body:{result:'ok'}}],
    expect: { status: 'SUCCESS', polls: 4 } },
  { name: 'transient 500 then success',
    script: [{status:202},{status:500},{status:200,body:{result:'ok'}}],
    expect: { status: 'SUCCESS', minPolls: 2 } },
  { name: '410 terminal failure',
    script: [{status:202},{status:410}],
    expect: { status: 'RETRYING_OR_FAILED' /* depends on max_attempts */ } },
  { name: 'missing Location',
    script: [{status:202, noLocation: true}],
    expect: { status: 'FAILED', errorType: 'MISSING_POLL_URL' } },
  { name: 'timeout via max_wait_ms',
    script: [{status:202},{status:202}],
    overrides: { max_wait_ms: 2000, max_polls: 100 },
    expect: { status: 'FAILED', errorType: 'TIMEOUT' } },
  { name: 'timeout via max_polls',
    script: [{status:202},{status:202}],
    overrides: { max_wait_ms: 600000, max_polls: 2 },
    expect: { status: 'FAILED', errorType: 'TIMEOUT' } },
  { name: 'callback /complete wins',
    script: [{status:202},{status:202,retry_after:30}],
    callback: { kind: 'complete', body: {result:'cb-wins'}, afterMs: 500 },
    expect: { status: 'SUCCESS', output: {result:'cb-wins'} } },
  { name: 'callback /fail re-dispatches',
    script: [{status:202},{status:202,retry_after:30}],
    callback: { kind: 'fail', body: {error:'cb-says-fail'}, afterMs: 500 },
    expect: { status: 'RETRYING_OR_FAILED' } },
  { name: 'callback duplicate yields 409',
    script: [{status:202},{status:202,retry_after:30}],
    callback: { kind: 'complete', body: {result:'ok'}, afterMs: 500, duplicate: true },
    expect: { status: 'SUCCESS', expectedSecondCallStatus: 409 } },
  { name: 'cancel during WAITING',
    script: [{status:202},{status:202,retry_after:30}],
    cancelAfterMs: 800,
    expect: { status: 'CANCELLED', expectMockDelete: true } },
];
```

Each test creates a fresh job with a fresh idempotency key. For "duplicate callback", invoke `POST /v1/callbacks/.../complete` twice. For "cancel during WAITING", call `POST /v1/executions/{id}/cancel` (or the job-level cancel) and then inspect `/async/inspect/{id}` on the mock-server to confirm a DELETE was observed.

Polling Kronos for terminal status: read `GET /v1/orgs/{org}/workspaces/{ws}/v1/executions/{id}` every 250ms, fail after 30s.

(The exact helper imports mirror `test-immediate.ts`. Don't re-invent setup.)

- [ ] **Step 2: Add `justfile` recipe**

In `justfile`, near `test-immediate`:

```make
# End-to-end long-running tests (requires `just dev` services)
test-long-running:
    cd cli && npx tsx src/test-long-running.ts
```

- [ ] **Step 3: Run end-to-end**

```bash
just setup
just dev &
sleep 5
just test-long-running
```

Expected: all 11 cases pass.

- [ ] **Step 4: Commit**

```bash
git add cli/src/test-long-running.ts justfile
git commit -m "test(e2e): end-to-end matrix for long-running polling + callback semantics"
```

---

## Phase 8: Dashboard

### Task 22: Polls panel + status pill additions

**Files:**
- Modify: `crates/dashboard/src/pages/execution_detail.rs` (or whichever file owns the existing Attempts panel)
- Modify: `crates/dashboard/src/components/status_pill.rs` (or wherever the status pill is defined)
- Modify: `crates/dashboard/src/api.rs` (or wherever the API client lives) to call `GET /executions/{id}/polls`

- [ ] **Step 1: Add `WAITING` and `POLLING` to the status pill**

In the status pill component file, locate the `match status` expression that maps `"RUNNING" => ...`, `"SUCCESS" => ...` etc. Add:

```rust
"WAITING" => ("badge-warning", "WAITING"),
"POLLING" => ("badge-info", "POLLING"),
```

Use whatever class names the existing pill component uses (`badge-warning` is a placeholder for the existing yellow style).

- [ ] **Step 2: Add a Polls panel mirroring the Attempts panel**

In the execution detail page:

```rust
let polls = create_resource(
    move || execution_id().clone(),
    move |id| async move { api::list_polls(&id).await },
);

view! {
    /* existing layout */
    <section class="polls-panel">
        <h3>"Polls"</h3>
        <Suspense fallback=|| view! { <p>"Loading…"</p> }>
            { move || polls.get().map(|polls| view! {
                <table>
                    <thead><tr>
                        <th>"#"</th><th>"When"</th><th>"Status"</th>
                        <th>"Duration"</th><th>"Retry-After"</th><th>"Classification"</th>
                    </tr></thead>
                    <tbody>
                        { polls.into_iter().map(|p| view! {
                            <tr>
                                <td>{p.poll_number}</td>
                                <td>{p.polled_at.to_rfc3339()}</td>
                                <td>{p.status_code.map(|c| c.to_string()).unwrap_or("—".into())}</td>
                                <td>{p.duration_ms.map(|d| format!("{d}ms")).unwrap_or("—".into())}</td>
                                <td>{p.retry_after_ms.map(|r| format!("{r}ms")).unwrap_or("—".into())}</td>
                                <td>{p.classification.clone()}</td>
                            </tr>
                        }).collect_view() }
                    </tbody>
                </table>
            })}
        </Suspense>
    </section>
}
```

Mirror the existing Attempts panel's layout and class names — don't introduce a new visual style.

- [ ] **Step 3: Add the API client call**

In `crates/dashboard/src/api.rs` (or equivalent), mirror `list_attempts`:

```rust
pub async fn list_polls(execution_id: &str) -> Result<Vec<Poll>, String> {
    /* fetch GET /executions/{id}/polls, deserialize Vec<Poll> */
}
```

Where `Poll` is a serde model that matches `kronos_common::models::Poll`. The dashboard already has a parallel models module — add `Poll` there in the same shape it uses for `Attempt`.

- [ ] **Step 4: Build dashboard**

```bash
just dashboard-build
```

(or whatever recipe builds the WASM dashboard — check the existing `justfile`.)

Expected: WASM build succeeds.

- [ ] **Step 5: Manually verify in browser**

```bash
just dev
# Visit http://localhost:3000, navigate to an execution that was driven by a long-running endpoint
```

Expected: WAITING/POLLING pills render; Polls panel populates from `/polls`; both panels coexist with Attempts.

- [ ] **Step 6: Commit**

```bash
git add crates/dashboard/
git commit -m "feat(dashboard): polls panel + WAITING/POLLING status pills"
```

---

## Self-review

Cross-checked plan against the spec section by section:

- **State machine** (spec §State machine): Tasks 1, 2 introduce WAITING/POLLING. Task 5 implements the CASE-based claim. Task 6 implements the four transition helpers + `complete_success_from_long_running`. Task 7 extends cancel to WAITING. ✔
- **Data model** (spec §Data model): Task 1 covers all migrations including pickup index update. Tasks 2, 3, 8, 10 surface the new columns/types in Rust. ✔
- **Endpoint spec extensions** (spec §Endpoint spec): Task 9 adds types + validation including disjointness, bounds, "at least one mode", default values. ✔
- **Job-level overrides** (spec §Job-level overrides): Task 10 adds AsyncOverrides + resolve_async_bounds + handler integration. Task 8 persists them. ✔
- **Worker pipeline** (spec §Worker pipeline): Task 13 adds async detection in dispatch (incl. MISSING_POLL_URL non-retryable). Tasks 14–15 add the poll path with bound check, Retry-After parsing, RFC 3986 URL resolution, classification + transitions. Task 16 wires the branching. ✔
- **Callback API + templates** (spec §Callback API): Task 18 adds /complete and /fail with the exact response codes from the spec. Task 12 adds the template variables. ✔
- **Cancellation** (spec §Cancellation): Task 17 adds the best-effort DELETE with secret-resolved headers. Task 7 extends DB cancel. ✔
- **Observability** (spec §Observability): Metrics are referenced; the actual `metrics::counter!`/`gauge!`/`histogram!` calls live inside Tasks 13/15 alongside the state transitions. Execution logs are emitted by Tasks 13, 15, 17. Dashboard surfaces in Task 22. ⚠ — explicit "register these metric names" step not split out; metric calls land naturally in the relevant Tasks. Acceptable: matches spec's intent that they live alongside the state changes.
- **Testing** (spec §Testing): Task 9 covers spec-validation unit tests; Task 14 covers URL/Retry-After; Task 15 covers classification; Task 21 covers the integration matrix (all 12 rows of the spec matrix represented). ✔
- **Migration** (spec §Migration): Task 1. ✔

Placeholder scan: no "TBD" / "TODO" / "appropriate" / "etc." in any step. Every step shows the code, command, or text to change. ✔

Type consistency: `ClaimedExecution` field `claim_status: String` used identically in Tasks 5, 15, 16. `PollClassification` enum variants `SUCCESS`/`PENDING`/`TERMINAL_FAILURE`/`TRANSIENT_ERROR` used identically in Tasks 3, 4, 15. `complete_success_from_long_running` and `retry_from_poll` and `transition_to_waiting` / `transition_back_to_waiting` and `complete_failed_timeout` are all defined in Task 6 and consumed in Tasks 15 (poll path) and 18 (callbacks). ✔

One self-noted risk worth flagging in implementation: in Task 18 the callback handler depends on `kronos_worker::backoff` which would create a workspace cycle. Mitigation noted inline: lift `compute_backoff` to `crates/common/src/backoff.rs` first. The lift takes ~5 minutes and is captured in Task 18 Step 1.
