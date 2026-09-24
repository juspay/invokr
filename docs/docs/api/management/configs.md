---
id: configs
title: Configs
title_meta: Configs API
---

# Configs

Configs are static, centrally managed variables available in endpoint spec templates via the `{{config.*}}` namespace. Define a value once and reference it from multiple endpoints instead of duplicating it in each job's input.

For example, an API base URL can be stored as a config and referenced in endpoint specs as `{{config.api_base_url}}`. When the config value changes, all endpoints that reference it automatically pick up the new value (after the cache TTL expires).

## Authentication and headers

All config endpoints require:

```
Authorization: Bearer <api_key>
X-Org-Id: <org_id>
X-Workspace-Id: <workspace_id>
```

:::warning
Config endpoints are tenant-scoped. Requests without `X-Org-Id` and `X-Workspace-Id` headers will be rejected.
:::

## Fields

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Unique config name within the workspace |
| `values` | object (JSON) | JSON object containing key-value pairs (e.g. `{"api_base_url": "https://...", "sender": "noreply@..."}`) |
| `created_at` | string (ISO 8601) | Creation timestamp |
| `updated_at` | string (ISO 8601) | Last update timestamp |

:::note
The `values` field must be a JSON object. Arrays, strings, numbers, and other JSON types are rejected with a `400 InvalidRequest` error.
:::

## Caching

Configs are cached in the worker using a `DashMap` with a configurable TTL (default: 60 seconds, controlled by `INVOKR_CONFIG_CACHE_TTL_SEC`). After the TTL expires, the next request for that config triggers a fresh database read.

Config updates can therefore take up to `INVOKR_CONFIG_CACHE_TTL_SEC` seconds to take effect across all workers.

## Template usage

Configs are referenced in endpoint specs via `{{config.*}}`:

```json
{
  "spec": {
    "url": "{{config.api_base_url}}/emails/welcome",
    "body_template": {
      "sender": "{{config.sender}}"
    }
  },
  "config": "email-service"
}
```

The endpoint's `config` field specifies which config to load. The `{{config.*}}` templates in the spec are then resolved against that config's `values` object at execution time.

See [Template resolution](../../core-concepts/templates) for full details.

## Endpoints

### Create config

```
POST /v1/configs
```

Creates a new config. Returns `409 Conflict` if a config with the same name already exists.

**Request body:**

```json
{
  "name": "email-service",
  "values": {
    "api_base_url": "https://api.myapp.com",
    "sender": "noreply@myapp.com",
    "timeout_sec": 30
  }
}
```

**Example:**

```bash
curl -X POST http://localhost:8080/v1/configs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..." \
  -H "Content-Type: application/json" \
  -d '{
    "name": "email-service",
    "values": {
      "api_base_url": "https://api.myapp.com",
      "sender": "noreply@myapp.com"
    }
  }'
```

**Response (`201 Created`):**

```json
{
  "data": {
    "name": "email-service",
    "values": {
      "api_base_url": "https://api.myapp.com",
      "sender": "noreply@myapp.com"
    },
    "created_at": "2026-06-20T10:15:00Z",
    "updated_at": "2026-06-20T10:15:00Z"
  }
}
```

**Error responses:**

| Status | Error | Description |
|--------|-------|-------------|
| `400` | `InvalidRequest` | `values` is not a JSON object |
| `409` | `Conflict` | Config with name already exists |

---

### List configs

```
GET /v1/configs?limit={limit}&cursor={cursor}
```

Returns configs with cursor-based pagination.

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `limit` | integer | 50 | Maximum number of items to return (max 100) |
| `cursor` | string | *(none)* | Pagination cursor from the previous response |

**Example:**

```bash
curl "http://localhost:8080/v1/configs?limit=10" \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..."
```

**Response (`200 OK`):**

```json
{
  "data": [
    {
      "name": "email-service",
      "values": {
        "api_base_url": "https://api.myapp.com",
        "sender": "noreply@myapp.com"
      },
      "created_at": "2026-06-20T10:15:00Z",
      "updated_at": "2026-06-20T10:15:00Z"
    },
    {
      "name": "sms-gateway",
      "values": {
        "api_base_url": "https://sms.example.com",
        "sender": "Invokr"
      },
      "created_at": "2026-06-20T10:20:00Z",
      "updated_at": "2026-06-20T10:20:00Z"
    }
  ],
  "cursor": "c21zLWdhdGV3YXk="
}
```

When `cursor` is present in the response, there are more items available. Pass it as the `cursor` query parameter in the next request. When `cursor` is `null`, all items have been returned.

:::info
The pagination cursor is an opaque string — do not construct it manually. It is typically a base64-encoded encoding of the last item's `name` field.
:::

---

### Get config

```
GET /v1/configs/{name}
```

Returns a single config by name.

**Example:**

```bash
curl http://localhost:8080/v1/configs/email-service \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..."
```

**Response (`200 OK`):**

```json
{
  "data": {
    "name": "email-service",
    "values": {
      "api_base_url": "https://api.myapp.com",
      "sender": "noreply@myapp.com"
    },
    "created_at": "2026-06-20T10:15:00Z",
    "updated_at": "2026-06-20T10:15:00Z"
  }
}
```

**Error responses:**

| Status | Error | Description |
|--------|-------|-------------|
| `404` | `ConfigNotFound` | Config not found |

---

### Update config

```
PUT /v1/configs/{name}
```

Updates an existing config's values. The entire `values` object is replaced.

**Request body:**

```json
{
  "values": {
    "api_base_url": "https://api-v2.myapp.com",
    "sender": "no-reply@myapp.com",
    "retry_count": 3
  }
}
```

**Example:**

```bash
curl -X PUT http://localhost:8080/v1/configs/email-service \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..." \
  -H "Content-Type: application/json" \
  -d '{
    "values": {
      "api_base_url": "https://api-v2.myapp.com",
      "sender": "no-reply@myapp.com"
    }
  }'
```

**Response (`200 OK`):**

```json
{
  "data": {
    "name": "email-service",
    "values": {
      "api_base_url": "https://api-v2.myapp.com",
      "sender": "no-reply@myapp.com"
    },
    "created_at": "2026-06-20T10:15:00Z",
    "updated_at": "2026-06-20T11:00:00Z"
  }
}
```

**Error responses:**

| Status | Error | Description |
|--------|-------|-------------|
| `400` | `InvalidRequest` | `values` is not a JSON object |
| `404` | `ConfigNotFound` | Config not found |

:::tip
Config updates take effect after the worker's cache TTL expires (`INVOKR_CONFIG_CACHE_TTL_SEC`, default 60 seconds). To force an immediate update, restart the worker.
:::

---

### Delete config

```
DELETE /v1/configs/{name}
```

Deletes a config. Returns `409 Conflict` if any endpoints reference this config (checked via `has_dependent_endpoints`).

**Example:**

```bash
curl -X DELETE http://localhost:8080/v1/configs/email-service \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..."
```

**Response (`204 No Content`):**

No response body.

**Error responses:**

| Status | Error | Description |
|--------|-------|-------------|
| `404` | `ConfigNotFound` | Config not found |
| `409` | `Conflict` | Config has dependent endpoints — remove or update the endpoint's `config` field first |

:::warning
You cannot delete a config that is referenced by any endpoint. First update or delete the dependent endpoints, then delete the config.
:::

## See also

- [Secrets](./secrets) — encrypted variables referenced via `{{secret.*}}`
- [Organizations](./organizations) — top-level tenant entity
- [Workspaces](./workspaces) — workspace creation and schema provisioning
- [Template resolution](../../core-concepts/templates) — how `{{config.*}}` templates are resolved
- [Environment Variables](../../configuration/environment-variables) — `INVOKR_CONFIG_CACHE_TTL_SEC`
