-- invokr:scope=tenant
--
-- Long-running jobs: WAITING / POLLING execution statuses, polls table,
-- per-execution and per-job async bounds.
--
-- Applies to ONE workspace schema, with `{p}` replaced by that workspace's
-- table prefix (the same substitution crates/common/migrations/workspace_v1.sql
-- gets in db/workspaces.rs). The scope marker above is what keeps it out of the
-- `public` run, which would apply it verbatim with `{p}` unsubstituted.
-- Apply it with `just db-migrate-tenant <schema> [prefix]`.
--
-- Idempotent by construction, so it is safe to run against any workspace:
--   * freshly provisioned from workspace_v1.sql -> no-op (v1 already has all of this)
--   * provisioned before this feature landed    -> upgrades it in place
--
-- Keep in sync by hand with crates/common/migrations/workspace_v1.sql: a new
-- workspace is built from v1 alone and never replays this file, so anything
-- added to one must be added to the other.

-- Extend executions status CHECK with the two long-running states.
-- DROP-then-ADD is the idempotent form: Postgres has no ADD CONSTRAINT IF NOT EXISTS.
ALTER TABLE {p}executions DROP CONSTRAINT IF EXISTS chk_{p}exec_status;
ALTER TABLE {p}executions ADD CONSTRAINT chk_{p}exec_status CHECK (status IN (
    'PENDING', 'QUEUED', 'RUNNING', 'RETRYING',
    'SUCCESS', 'FAILED', 'CANCELLED',
    'WAITING', 'POLLING'
));

-- A dispatch that parks the execution records its attempt as WAITING
ALTER TABLE {p}attempts DROP CONSTRAINT IF EXISTS chk_{p}attempt_status;
ALTER TABLE {p}attempts ADD CONSTRAINT chk_{p}attempt_status CHECK (status IN (
    'SUCCESS', 'FAILED', 'WAITING'
));

-- Long-running columns on executions (snapshot of effective values + runtime state)
ALTER TABLE {p}executions
    ADD COLUMN IF NOT EXISTS poll_url            TEXT,
    ADD COLUMN IF NOT EXISTS poll_count          INT         NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS polling_started_at  TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS polling_deadline    TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS max_wait_ms         BIGINT,
    ADD COLUMN IF NOT EXISTS max_polls           INT;

-- Extend pickup index to include WAITING
DROP INDEX IF EXISTS idx_{p}executions_pickup;
CREATE INDEX IF NOT EXISTS idx_{p}executions_pickup
    ON {p}executions (status, run_at ASC)
    WHERE status IN ('QUEUED', 'RETRYING', 'PENDING', 'WAITING');

-- Per-job async overrides (resolved at job creation; copied to executions on insert)
ALTER TABLE {p}jobs
    ADD COLUMN IF NOT EXISTS async_max_wait_ms   BIGINT,
    ADD COLUMN IF NOT EXISTS async_max_polls     INT;

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
