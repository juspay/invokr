---
id: multi-tenancy
title: Multi-Tenancy
---

# Multi-Tenancy

Invokr uses **schema-per-tenant** isolation. Each workspace gets its own PostgreSQL schema with isolated tables. Shared tables live in the `public` schema. Each tenant is fully isolated: its jobs, executions, endpoints, and all other resources are scoped to the workspace's own database schema.

---

## Organizations and workspaces

Invokr has a two-level tenant hierarchy:

| Entity | Schema | Description |
|--------|--------|-------------|
| **Organization** | `public` | Top-level tenant entity. Contains one or more workspaces. |
| **Workspace** | `public` | A unit of isolation within an organization. Each workspace gets its own database schema. |

```text
Organization: "My Company" (org_id: 550e8400-...)
  ├── Workspace: "Production" (workspace_id: 660e8400-... → schema: my_company_production)
  ├── Workspace: "Staging"    (workspace_id: 770e8400-... → schema: my_company_staging)
  └── Workspace: "Development"(workspace_id: 880e8400-... → schema: my_company_development)
```

Organizations and workspaces are stored in the `public` schema and are accessible without tenant headers. All other resources are tenant-scoped.

---

## Tenant-scoped tables

Each workspace gets its own PostgreSQL schema containing all tenant-scoped tables:

```text
public schema:        organizations, workspaces
tenant schema:        payload_specs, configs, secrets, endpoints,
(org_workspace):      jobs, executions, attempts, execution_logs
```

| Schema | Tables |
|--------|--------|
| `public` | `organizations`, `workspaces` |
| `{org_slug}_{workspace_slug}` | `payload_specs`, `configs`, `secrets`, `endpoints`, `jobs`, `executions`, `attempts`, `execution_logs` |

:::info
The schema name is derived from the organization slug and workspace slug: `{org_slug}_{workspace_slug}`. For example, the "Production" workspace in "My Company" (slug: `my-company`) would get the schema `my_company_production`.
:::

---

## X-Org-Id and X-Workspace-Id headers

All tenant-scoped API requests require two headers:

| Header | Description | Format |
|--------|-------------|--------|
| `X-Org-Id` | Organization identifier | UUID or slug |
| `X-Workspace-Id` | Workspace identifier | UUID or slug |

```bash
curl -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: 550e8400-e29b-41d4-a716-446655440000" \
  -H "X-Workspace-Id: 660e8400-e29b-41d4-a716-446655440000" \
  -H "Content-Type: application/json" \
  -d '{ ... }'
```

Or using slugs:

```bash
curl -X POST http://localhost:8080/v1/jobs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: my-company" \
  -H "X-Workspace-Id: production" \
  -H "Content-Type: application/json" \
  -d '{ ... }'
```

:::note
Only the `POST /v1/orgs` and `POST /v1/orgs/{org_id}/workspaces` endpoints (and their list/get/update operations) do **not** require the `X-Workspace-Id` header. Everything else is tenant-scoped.
:::

---

## Schema resolution

The `X-Org-Id` and `X-Workspace-Id` headers accept either a **UUID** or a **slug**:

| Input | Resolution |
|-------|------------|
| UUID (e.g. `550e8400-e29b-41d4-a716-446655440000`) | Direct lookup in `public.workspaces` by `workspace_id` |
| Slug (e.g. `production`) | Lookup in `public.workspaces` by slug within the org |

The resolved schema name is cached by the `SchemaRegistry` to avoid repeated database lookups.

---

## SchemaRegistry

The `SchemaRegistry` is a cached mapping of workspace IDs to schema names, used by both the API server and the worker:

| Property | Value |
|----------|-------|
| Cache TTL | 30 seconds |
| Storage | In-memory per process |
| Purpose | Map `workspace_id` → PostgreSQL schema name |
| Used by | API server (for `scoped_connection` / `scoped_transaction`), Worker (for iterating tenant schemas) |

The registry is refreshed lazily — on cache miss or TTL expiry, the registry queries the `public.workspaces` table to resolve the schema name.

---

## scoped_connection / scoped_transaction

When a tenant-scoped request arrives, the API server resolves the workspace's schema name and sets the PostgreSQL `search_path` to include that schema. This is done via:

- **`scoped_connection`** — acquires a connection from the pool and sets `search_path` to the workspace schema
- **`scoped_transaction`** — acquires a connection, sets `search_path`, and begins a transaction

All SQL queries within the scope then target the correct tenant schema automatically. Each query operates within the workspace's schema only, so cross-tenant data leakage is not possible.

```sql
-- Before any tenant-scoped query:
SET search_path TO my_company_production, public;
```

:::info
The `public` schema is always included in the `search_path` so that shared tables (like `organizations` and `workspaces`) remain accessible. However, tenant-scoped tables (`jobs`, `endpoints`, etc.) are only accessible within the workspace's own schema.
:::

---

## Worker iteration over tenant schemas

The worker does not receive tenant headers. Instead, it iterates all active workspace schemas:

1. The worker queries the `SchemaRegistry` (cached, 30s TTL) for all active workspace schemas
2. For each poll cycle, the worker iterates all active schemas
3. Within each schema, it claims executions via `SELECT FOR UPDATE SKIP LOCKED`
4. Each claimed execution is processed in the context of its workspace's schema

```text
Worker Poll Cycle:
  ├── Acquire semaphore permit
  ├── For each active workspace schema:
  │     ├── SELECT ... FOR UPDATE SKIP LOCKED
  │     └── If execution claimed → spawn task
  ├── If no work found → release permit, sleep poll_interval
  └── Repeat
```

:::tip
When scaling workers horizontally, each worker independently iterates all tenant schemas. With `SKIP LOCKED`, no two workers claim the same execution; locked rows are skipped.
:::

---

## Workspace creation

Creating a workspace provisions the database schema and all tenant-scoped tables:

1. **Insert workspace record** into `public.workspaces` with the org ID, name, and slug
2. **Create PostgreSQL schema** (e.g. `CREATE SCHEMA my_company_production`)
3. **Create tenant-scoped tables** within the new schema (jobs, executions, endpoints, etc.)
4. **Create indexes** (pickup index, idempotency indexes, etc.)
5. **Register the reaper** — a pg_cron entry for the workspace's schema that runs the dogfooded CRON sweep

```bash
curl -X POST http://localhost:8080/v1/orgs/{org_id}/workspaces \
  -H "Authorization: Bearer dev-api-key" \
  -H "Content-Type: application/json" \
  -d '{ "name": "Production", "slug": "production" }'
```

:::note
The reaper is Invokr's own dogfooded CRON sweep that retires expired CRON jobs and unschedules their pg_cron entries. The reaper's schedule is controlled by `INVOKR_REAPER_CRON_EXPRESSION` (default: `*/15 * * * *` — every 15 minutes). This expression is baked into each workspace's pg_cron entry at creation time, so changing it only affects workspaces created afterward.
:::

---

## Isolation guarantees

The schema-per-tenant model provides strong isolation guarantees:

| Guarantee | How |
|-----------|-----|
| **Data isolation** | Each workspace has its own schema. Queries are scoped via `search_path`. No cross-tenant data access is possible. |
| **Resource isolation** | Endpoints, jobs, configs, and secrets are per-workspace. No shared resources between tenants. |
| **Performance isolation** | Queries within one workspace's schema don't contend with another workspace's data. PostgreSQL query planner optimizes per-schema. |
| **Operational isolation** | Workspace creation and deletion are independent. Migrating or backing up one workspace doesn't affect others. |

:::warning
While schemas provide logical isolation, all workspaces share the same PostgreSQL instance and connection pool. A workspace with very high job volume can consume more database resources (connections, CPU, I/O). For true physical isolation, use separate PostgreSQL instances per workspace.
:::

---

## See also

- [The Three-Step Workflow](./overview) — how multi-tenancy fits into the overall model
- [Jobs](./jobs) — tenant-scoped job creation
- [Endpoints](./endpoints) — tenant-scoped endpoint registration
- [Environment Variables](../configuration/environment-variables) — `INVOKR_REAPER_CRON_EXPRESSION`, `INVOKR_DB_POOL_SIZE`
- [Quickstart](../quickstart) — creating an org and workspace
