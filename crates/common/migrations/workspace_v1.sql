-- Workspace-scoped tables.
-- The {p} placeholder is replaced at runtime with the configured table prefix used as-is,
-- including any trailing separator (e.g. "sched_"), or empty string for no prefix.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE IF NOT EXISTS {p}payload_specs (
    name          TEXT        NOT NULL,
    schema_json   JSONB       NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_{p}payload_specs PRIMARY KEY (name)
);

CREATE TABLE IF NOT EXISTS {p}configs (
    name          TEXT        NOT NULL,
    values_json   JSONB       NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_{p}configs PRIMARY KEY (name)
);

CREATE TABLE IF NOT EXISTS {p}secrets (
    name              TEXT        NOT NULL,
    encrypted_value   BYTEA       NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_{p}secrets PRIMARY KEY (name)
);

CREATE TABLE IF NOT EXISTS {p}endpoints (
    name              TEXT        NOT NULL,
    endpoint_type     TEXT        NOT NULL,
    payload_spec_ref  TEXT,
    config_ref        TEXT,
    spec              JSONB       NOT NULL,
    retry_policy      JSONB,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_{p}endpoints PRIMARY KEY (name),
    CONSTRAINT fk_{p}endpoints_payload_spec FOREIGN KEY (payload_spec_ref) REFERENCES {p}payload_specs (name),
    CONSTRAINT fk_{p}endpoints_config FOREIGN KEY (config_ref) REFERENCES {p}configs (name),
    CONSTRAINT chk_{p}endpoint_type CHECK (endpoint_type IN ('HTTP', 'KAFKA', 'REDIS_STREAM', 'INTERNAL'))
);

CREATE INDEX IF NOT EXISTS idx_{p}endpoints_type ON {p}endpoints (endpoint_type);

CREATE TABLE IF NOT EXISTS {p}jobs (
    job_id                TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    endpoint              TEXT        NOT NULL,
    endpoint_type         TEXT        NOT NULL,
    trigger_type          TEXT        NOT NULL,
    status                TEXT        NOT NULL DEFAULT 'ACTIVE',
    version               BIGINT      NOT NULL DEFAULT 1,
    previous_version_id   TEXT,
    replaced_by_id        TEXT,
    idempotency_key       TEXT,
    input                 JSONB,
    run_at                TIMESTAMPTZ,
    cron_expression       TEXT,
    cron_timezone         TEXT,
    cron_starts_at        TIMESTAMPTZ,
    cron_ends_at          TIMESTAMPTZ,
    cron_next_run_at      TIMESTAMPTZ,
    cron_last_tick_at     TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    retired_at            TIMESTAMPTZ,
    async_max_wait_ms     BIGINT,
    async_max_polls       INT,
    CONSTRAINT pk_{p}jobs PRIMARY KEY (job_id),
    CONSTRAINT fk_{p}jobs_endpoint FOREIGN KEY (endpoint) REFERENCES {p}endpoints (name),
    CONSTRAINT chk_{p}trigger_type CHECK (trigger_type IN ('IMMEDIATE', 'DELAYED', 'CRON')),
    CONSTRAINT chk_{p}job_status CHECK (status IN ('ACTIVE', 'RETIRED')),
    CONSTRAINT chk_{p}job_endpoint_type CHECK (endpoint_type IN ('HTTP', 'KAFKA', 'REDIS_STREAM', 'INTERNAL'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_{p}jobs_idempotency
    ON {p}jobs (endpoint, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_{p}jobs_cron_due
    ON {p}jobs (cron_next_run_at)
    WHERE trigger_type = 'CRON' AND status = 'ACTIVE';

CREATE INDEX IF NOT EXISTS idx_{p}jobs_endpoint ON {p}jobs (endpoint, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_{p}jobs_status   ON {p}jobs (status,   created_at DESC);

CREATE TABLE IF NOT EXISTS {p}executions (
    execution_id    TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    job_id          TEXT        NOT NULL,
    endpoint        TEXT        NOT NULL,
    endpoint_type   TEXT        NOT NULL,
    idempotency_key TEXT,
    status          TEXT        NOT NULL DEFAULT 'PENDING',
    input           JSONB,
    output          JSONB,
    attempt_count   BIGINT      NOT NULL DEFAULT 0,
    max_attempts    BIGINT      NOT NULL DEFAULT 1,
    worker_id       TEXT,
    run_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at      TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    duration_ms     BIGINT,
    poll_url            TEXT,
    poll_count          INT         NOT NULL DEFAULT 0,
    polling_started_at  TIMESTAMPTZ,
    polling_deadline    TIMESTAMPTZ,
    max_wait_ms         BIGINT,
    max_polls           INT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_{p}executions PRIMARY KEY (execution_id),
    CONSTRAINT fk_{p}executions_job FOREIGN KEY (job_id) REFERENCES {p}jobs (job_id),
    CONSTRAINT chk_{p}exec_status CHECK (status IN (
        'PENDING', 'QUEUED', 'RUNNING', 'RETRYING',
        'SUCCESS', 'FAILED', 'CANCELLED',
        'WAITING', 'POLLING'
    ))
);

CREATE INDEX IF NOT EXISTS idx_{p}executions_pickup
    ON {p}executions (status, run_at ASC)
    WHERE status IN ('QUEUED', 'RETRYING', 'PENDING', 'WAITING');

CREATE UNIQUE INDEX IF NOT EXISTS idx_{p}executions_cron_dedup
    ON {p}executions (job_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_{p}executions_by_job  ON {p}executions (job_id,  created_at DESC);
CREATE INDEX IF NOT EXISTS idx_{p}executions_running ON {p}executions (status,   started_at)
    WHERE status = 'RUNNING';

CREATE TABLE IF NOT EXISTS {p}attempts (
    attempt_id      TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    execution_id    TEXT        NOT NULL,
    attempt_number  BIGINT      NOT NULL,
    status          TEXT        NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL,
    completed_at    TIMESTAMPTZ,
    duration_ms     BIGINT,
    output          JSONB,
    error           JSONB,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_{p}attempts PRIMARY KEY (attempt_id),
    CONSTRAINT fk_{p}attempts_execution FOREIGN KEY (execution_id) REFERENCES {p}executions (execution_id),
    CONSTRAINT uq_{p}attempts_exec_number UNIQUE (execution_id, attempt_number),
    CONSTRAINT chk_{p}attempt_status CHECK (status IN ('SUCCESS', 'FAILED', 'WAITING'))
);

CREATE INDEX IF NOT EXISTS idx_{p}attempts_by_execution
    ON {p}attempts (execution_id, attempt_number ASC);

CREATE TABLE IF NOT EXISTS {p}execution_logs (
    log_id          TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    execution_id    TEXT        NOT NULL,
    attempt_number  BIGINT      NOT NULL,
    level           TEXT        NOT NULL,
    message         TEXT        NOT NULL,
    logged_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_{p}execution_logs PRIMARY KEY (log_id),
    CONSTRAINT fk_{p}logs_execution FOREIGN KEY (execution_id) REFERENCES {p}executions (execution_id),
    CONSTRAINT chk_{p}log_level CHECK (level IN ('DEBUG', 'INFO', 'WARN', 'ERROR'))
);

CREATE INDEX IF NOT EXISTS idx_{p}logs_by_execution
    ON {p}execution_logs (execution_id, logged_at ASC);
CREATE INDEX IF NOT EXISTS idx_{p}logs_by_attempt
    ON {p}execution_logs (execution_id, attempt_number, logged_at ASC);

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
