---
id: endpoints
title: Endpoints
---

# Endpoints

An endpoint is a registered delivery target definition. It specifies where to deliver a message, how to build it, and how to handle failures. Endpoints are created once and invoked by many jobs.

---

## What is an endpoint?

An endpoint defines:
- **Type**: the transport protocol (`HTTP`, `KAFKA`, `REDIS_STREAM`, or `INTERNAL`)
- **Payload spec reference**: optional JSON Schema for input validation
- **Config reference**: optional static variables available as `{{config.*}}` at execution time
- **Spec**: transport-specific configuration (URL, method, headers, body template for HTTP; bootstrap servers, topic, key/value templates for Kafka; etc.)
- **Retry policy**: how failures should be retried

Endpoints reference payload specs and configs by name. These references are validated at registration time. If the referenced payload spec or config doesn't exist, the API returns `422`.

---

## Endpoint types

| Type | Description | Dispatcher |
|------|-------------|------------|
| `HTTP` | Deliver to an HTTP endpoint via `reqwest`. Supports URL, method, headers, body template, timeout, and expected status codes. | `reqwest::Client` (shared, keep-alive pool) |
| `KAFKA` | Produce a message to a Kafka topic via `rdkafka`. Supports bootstrap servers, topic, key/value templates, headers, acks, and timeout. | `rdkafka::FutureProducer` (shared) |
| `REDIS_STREAM` | Add an entry to a Redis Stream via `redis`. Supports Redis URL, stream name, fields template, MAXLEN trimming, and timeout. | `redis::aio::ConnectionManager` (pooled) |
| `INTERNAL` | Invokr-internal endpoints (dogfooded). Used by the reaper for CRON sweep operations. User-created jobs with `INTERNAL` endpoints are rejected. | Internal |

:::info
Kafka and Redis Stream dispatchers are behind Cargo feature flags. Build with `cargo build --workspace --features invokr-worker/kafka` or `cargo build --workspace --features invokr-worker/redis-stream` to enable them.
:::

---

## Endpoint fields

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `name` | string | yes | Unique, URL-safe identifier (lowercase alphanumeric, hyphens). |
| `type` | string | yes | `HTTP`, `KAFKA`, `REDIS_STREAM`, or `INTERNAL`. |
| `payload_spec` | string | no | Name of a registered payload spec. Enables input validation at job creation time. |
| `config` | string | no | Name of a registered config. Values available as `{{config.*}}` in the endpoint spec. |
| `spec` | object | yes | Transport-specific configuration. See below. |
| `retry_policy` | object | no | Retry behavior on failure. See [Retry Policy](./retry-policy). |
| `created_at` | ISO 8601 | auto | Creation timestamp. |
| `updated_at` | ISO 8601 | auto | Last update timestamp. |

### HTTP `spec` fields

| Field | Type | Required | Default | Description |
|-------|------|:--------:|:-------:|-------------|
| `url` | string | yes | | Target URL. Supports `{{config.*}}`, `{{secret.*}}`. |
| `method` | string | yes | `POST` | `GET`, `POST`, `PUT`, `PATCH`, `DELETE`. |
| `headers` | object | no | | Key-value header map. Supports `{{config.*}}`, `{{secret.*}}`. |
| `body_template` | object | no | | JSON body. Supports `{{input.*}}`, `{{config.*}}`, `{{secret.*}}`. |
| `body` | object | no | | Static JSON body (no template resolution). Used if `body_template` is absent. |
| `timeout_ms` | integer | yes | `5000` | Request timeout in milliseconds. |
| `expected_status_codes` | integer[] | no | `[200, 201, 202, 204]` | Status codes treated as success. |

### Kafka `spec` fields

| Field | Type | Required | Default | Description |
|-------|------|:--------:|:-------:|-------------|
| `bootstrap_servers` | string | yes | | Kafka broker addresses. Supports `{{config.*}}`, `{{secret.*}}`. |
| `topic` | string | yes | | Target topic. Supports `{{config.*}}`. |
| `key_template` | string | no | | Message key. Supports `{{input.*}}`, `{{config.*}}`. |
| `value_template` | object | yes | | Message value. Supports `{{input.*}}`, `{{config.*}}`, `{{secret.*}}`. |
| `headers` | object | no | | Message headers. Supports `{{config.*}}`, `{{secret.*}}`. |
| `acks` | string | no | `"all"` | `"0"`, `"1"`, or `"all"`. |
| `timeout_ms` | integer | no | `10000` | Produce timeout in milliseconds. |

### Redis Stream `spec` fields

| Field | Type | Required | Default | Description |
|-------|------|:--------:|:-------:|-------------|
| `redis_url` | string | yes | | Redis connection URL. Supports `{{config.*}}`, `{{secret.*}}`. |
| `stream` | string | yes | | Target stream name. Supports `{{config.*}}`. |
| `fields_template` | object | yes | | Stream entry fields. Supports `{{input.*}}`, `{{config.*}}`, `{{secret.*}}`. |
| `max_len` | integer | no | | MAXLEN for trimming. |
| `approximate_trimming` | boolean | no | `true` | Use approximate (`~`) MAXLEN trimming. |
| `timeout_ms` | integer | no | `3000` | Operation timeout in milliseconds. |

---

## How endpoints reference payload specs and configs

Endpoints reference payload specs and configs **by name** (string reference, not ID):

- `payload_spec`: The name of a registered payload spec. When set, every job's `input` is validated against the payload spec's JSON Schema at creation time. If validation fails, the API returns `422 INPUT_VALIDATION_FAILED`.
- `config`: The name of a registered config. All key-value pairs in the config are available as `{{config.*}}` template variables in the endpoint spec.

These references are validated at endpoint creation time. If the referenced payload spec or config doesn't exist, the API returns `422 INVALID_PAYLOAD_SPEC_REF` or `422 INVALID_CONFIG_REF`.

:::warning
Deleting a payload spec or config that is referenced by an endpoint returns `409 CONFLICT`. You must remove the reference from the endpoint first.
:::

---

## The INTERNAL endpoint type

The `INTERNAL` endpoint type is used by Invokr itself for dogfooded operations. The primary use case is the **reaper** — a CRON sweep that retires expired CRON jobs and unschedules their pg_cron entries.

:::danger
User-created jobs with `INTERNAL` type endpoints are **rejected** by the API. The `INTERNAL` type is reserved for Invokr's own operations.
:::

---

## Creating an endpoint

### HTTP endpoint example

```bash
curl -X POST http://localhost:8080/v1/endpoints \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "send-welcome-email",
    "type": "HTTP",
    "payload_spec": "order-input",
    "config": "email-service",
    "spec": {
      "url": "{{config.api_base_url}}/emails/welcome",
      "method": "POST",
      "headers": {
        "Authorization": "Bearer {{secret.email_api_key}}",
        "Content-Type": "application/json"
      },
      "body_template": {
        "order_id": "{{input.order_id}}",
        "sender": "{{config.sender}}"
      },
      "timeout_ms": 5000,
      "expected_status_codes": [200, 201, 202, 204]
    },
    "retry_policy": {
      "max_attempts": 3,
      "backoff": "exponential",
      "initial_delay_ms": 1000,
      "max_delay_ms": 30000
    }
  }'
```

Response (`201 Created`):

```json
{
  "name": "send-welcome-email",
  "type": "HTTP",
  "payload_spec": "order-input",
  "config": "email-service",
  "spec": { ... },
  "retry_policy": { ... },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

### Kafka endpoint example

```bash
curl -X POST http://localhost:8080/v1/endpoints \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "publish-order-event",
    "type": "KAFKA",
    "payload_spec": "order-event-input",
    "config": "order-events",
    "spec": {
      "bootstrap_servers": "{{config.bootstrap_servers}}",
      "topic": "{{config.topic}}",
      "key_template": "{{input.order_id}}",
      "value_template": {
        "event_type": "{{input.event_type}}",
        "order_id": "{{input.order_id}}",
        "amount": "{{input.amount}}"
      },
      "headers": {
        "ce-type": "order.{{input.event_type}}",
        "ce-source": "task-executor"
      },
      "acks": "all",
      "timeout_ms": 10000
    },
    "retry_policy": {
      "max_attempts": 5,
      "backoff": "exponential",
      "initial_delay_ms": 500,
      "max_delay_ms": 15000
    }
  }'
```

### Redis Stream endpoint example

```bash
curl -X POST http://localhost:8080/v1/endpoints \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "push-notification",
    "type": "REDIS_STREAM",
    "payload_spec": "notification-input",
    "config": "notification-stream",
    "spec": {
      "redis_url": "{{config.redis_url}}",
      "stream": "{{config.stream_name}}",
      "fields_template": {
        "user_id": "{{input.user_id}}",
        "title": "{{input.title}}",
        "body": "{{input.body}}"
      },
      "max_len": 1000,
      "approximate_trimming": true,
      "timeout_ms": 3000
    },
    "retry_policy": {
      "max_attempts": 3,
      "backoff": "exponential",
      "initial_delay_ms": 500,
      "max_delay_ms": 10000
    }
  }'
```

---

## Managing endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/endpoints` | Register an endpoint |
| `GET` | `/v1/endpoints` | List all endpoints |
| `GET` | `/v1/endpoints/{name}` | Get an endpoint |
| `PUT` | `/v1/endpoints/{name}` | Update an endpoint (applies to future jobs only) |
| `DELETE` | `/v1/endpoints/{name}` | Delete an endpoint (fails if active jobs reference it) |

:::note
Updating an endpoint does not affect in-flight executions. The update applies to future jobs only. In-flight executions use the endpoint definition from when they were claimed.
:::

---

## See also

- [Payload Specs](./payload-specs) — input validation via JSON Schema
- [Configs](./configs) — static variables for endpoints
- [Secrets](./secrets) — encrypted values for endpoints
- [Templates](./templates) — how `{{input.*}}`, `{{config.*}}`, and `{{secret.*}}` are resolved
- [Retry Policy](./retry-policy) — backoff strategies
- [HTTP Endpoints Guide](../guides/http-endpoints) — detailed HTTP endpoint walkthrough
- [Kafka Endpoints Guide](../guides/kafka-endpoints) — Kafka endpoint configuration
- [Redis Stream Endpoints Guide](../guides/redis-stream-endpoints) — Redis Stream endpoint configuration
