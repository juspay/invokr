---
id: configs
title: Configs
---

# Configs

Configs are key-value objects holding static variables available in endpoint specs at execution time. They keep configuration such as base URLs, topic names, and sender addresses separate from endpoint definitions, so you can update it without touching endpoints.

---

## What are configs?

A config is a named collection of key-value pairs. When an endpoint references a config, all its values become available as `{{config.*}}` template variables in the endpoint's spec. Configs are resolved by the worker at execution time.

Use cases for configs include:
- API base URLs (`{{config.api_base_url}}`)
- Email sender addresses (`{{config.sender}}`)
- Kafka bootstrap servers and topic names (`{{config.bootstrap_servers}}`, `{{config.topic}}`)
- Redis connection URLs and stream names (`{{config.redis_url}}`, `{{config.stream_name}}`)
- Feature flags or limits (`{{config.max_stream_length}}`)

---

## Creating a config

```bash
curl -X POST http://localhost:8080/v1/configs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "email-service",
    "values": {
      "api_base_url": "https://api.myapp.com",
      "sender": "noreply@myapp.com"
    }
  }'
```

Response (`201 Created`):

```json
{
  "name": "email-service",
  "values": {
    "api_base_url": "https://api.myapp.com",
    "sender": "noreply@myapp.com"
  },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

### Kafka config example

```json
{
  "name": "order-events",
  "values": {
    "bootstrap_servers": "kafka-1:9092,kafka-2:9092",
    "topic": "order.events.v1"
  }
}
```

### Redis Stream config example

```json
{
  "name": "notification-stream",
  "values": {
    "redis_url": "redis://redis-cluster:6379",
    "stream_name": "notifications:outbound",
    "max_stream_length": 100000
  }
}
```

---

## Config caching

Configs are cached in the worker process to avoid hitting the database on every execution:

| Property | Value |
|----------|-------|
| Cache implementation | `DashMap<String, (Config, Instant)>` |
| TTL | 60 seconds (configurable via `INVOKR_CONFIG_CACHE_TTL_SEC`) |
| Eviction | Lazy — entries are refreshed on next access after TTL expires |
| Storage | In-memory per worker process |

:::info
Config updates take effect for future executions after the cache TTL expires. In-flight executions use the config snapshot from when they started. There can be up to a 60-second delay between updating a config and the change appearing in executions.
:::

---

## How endpoints reference configs

Endpoints reference configs by name via the `config` field:

```json
{
  "name": "send-welcome-email",
  "type": "HTTP",
  "config": "email-service",
  "spec": {
    "url": "{{config.api_base_url}}/emails/welcome",
    "body_template": {
      "sender": "{{config.sender}}"
    }
  }
}
```

The reference is validated at endpoint creation time. If the config doesn't exist, the API returns `422 INVALID_CONFIG_REF`.

:::warning
You cannot delete a config that is referenced by an endpoint. The API returns `409 CONFLICT`. Remove the reference from all endpoints before deleting.
:::

---

## Template resolution with `{{config.*}}`

When the worker processes an execution, it resolves all `{{config.*}}` templates in the endpoint spec by looking up the key in the referenced config's `values` object:

| Template | Config values | Resolved value |
|----------|-------------|----------------|
| `{{config.api_base_url}}` | `{"api_base_url": "https://api.myapp.com"}` | `"https://api.myapp.com"` |
| `{{config.sender}}` | `{"sender": "noreply@myapp.com"}` | `"noreply@myapp.com"` |
| `{{config.topic}}` | `{"topic": "order.events.v1"}` | `"order.events.v1"` |

Templates can appear in URL strings, header values, and body template fields. The template engine walks the entire JSON tree recursively. See [Templates](./templates) for the full resolution engine details.

:::tip
When a template is the **entire** string value (e.g. `"{{config.api_base_url}}"`), the resolved value preserves its native JSON type. When templates are embedded in surrounding text (e.g. `"{{config.api_base_url}}/emails/welcome"`), the result is always a string (string interpolation).
:::

---

## Managing configs

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/configs` | Create a config |
| `GET` | `/v1/configs` | List all configs |
| `GET` | `/v1/configs/{name}` | Get a config |
| `PUT` | `/v1/configs/{name}` | Update a config |
| `DELETE` | `/v1/configs/{name}` | Delete (fails if endpoints reference it) |

:::note
Config updates take effect for future executions. In-flight executions use the config snapshot from when they started. Due to caching, there may be a delay of up to `INVOKR_CONFIG_CACHE_TTL_SEC` (default: 60s) before updates are visible to the worker.
:::

---

## See also

- [Endpoints](./endpoints) — how configs are referenced
- [Secrets](./secrets) — encrypted counterpart to configs
- [Templates](./templates) — the template resolution engine
- [Environment Variables](../configuration/environment-variables) — `INVOKR_CONFIG_CACHE_TTL_SEC`
- [The Three-Step Workflow](./overview) — where configs fit in the model
