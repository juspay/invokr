CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS pg_cron;
GRANT USAGE ON SCHEMA cron TO invokr_db_rw;

CREATE TABLE IF NOT EXISTS public.region_heartbeats (
    region        TEXT          NOT NULL,
    component     TEXT          NOT NULL,
    last_beat_at  TIMESTAMPTZ   NOT NULL DEFAULT now(),
    status        TEXT          NOT NULL DEFAULT 'ALIVE',
    metadata      JSONB,
    CONSTRAINT pk_region_heartbeats PRIMARY KEY (region, component)
);

CREATE TABLE IF NOT EXISTS public.region_status (
    region        TEXT          NOT NULL,
    alive         BOOL          NOT NULL DEFAULT true,
    failed_at     TIMESTAMPTZ,
    adopted_by    TEXT,
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT now(),
    CONSTRAINT pk_region_status PRIMARY KEY (region)
);

INSERT INTO public.region_status (region, alive, updated_at) VALUES ('default', true, now())
ON CONFLICT (region) DO NOTHING;

CREATE TABLE IF NOT EXISTS public.organizations (
    org_id      TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    name        TEXT        NOT NULL,
    slug        TEXT        NOT NULL UNIQUE,
    status      TEXT        NOT NULL DEFAULT 'ACTIVE',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_organizations PRIMARY KEY (org_id),
    CONSTRAINT chk_org_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'DELETED'))
);

CREATE TABLE IF NOT EXISTS public.workspaces (
    workspace_id    TEXT        NOT NULL DEFAULT gen_random_uuid()::TEXT,
    org_id          TEXT        NOT NULL,
    name            TEXT        NOT NULL,
    slug            TEXT        NOT NULL,
    schema_name     TEXT        NOT NULL UNIQUE,
    status          TEXT        NOT NULL DEFAULT 'ACTIVE',
    schema_version  BIGINT      NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_workspaces PRIMARY KEY (workspace_id),
    CONSTRAINT fk_workspaces_org FOREIGN KEY (org_id) REFERENCES public.organizations (org_id),
    CONSTRAINT uq_workspace_slug UNIQUE (org_id, slug),
    CONSTRAINT chk_ws_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'DELETED'))
);

CREATE INDEX IF NOT EXISTS idx_workspaces_org ON public.workspaces (org_id);
CREATE INDEX IF NOT EXISTS idx_workspaces_status ON public.workspaces (status);
