---
id: templates
title: Templates
---

# Templates

The template resolution engine builds endpoint specs at execution time. Template variables, enclosed in `{{ }}`, are resolved from four namespaces, so endpoint definitions can reference job input, configs, secrets, and execution metadata.

---

## Template resolution engine

When a worker claims an execution, it resolves all template variables in the endpoint's spec before dispatching. The resolution engine:

1. Loads the endpoint definition
2. Loads the referenced config (cached, 60s TTL) and secrets (cached, 300s TTL, encrypted at rest)
3. Resolves `{{config.*}}` and `{{secret.*}}` first
4. Resolves `{{input.*}}` from the execution's input payload
5. Resolves `{{execution.*}}` from execution metadata
6. Walks the entire JSON tree recursively — objects, arrays, and nested keys
7. Replaces every `{{namespace.key}}` occurrence

:::danger
If any variable is unresolvable, the execution fails immediately with `TEMPLATE_RESOLUTION_FAILED`. It is not retried, since the same template would fail identically on every attempt. Failing fast this way avoids wasted retries on configuration errors.
:::

---

## The four namespaces

| Namespace | Source | Resolved when | Per-execution? | Example |
|-----------|--------|---------------|----------------|---------|
| `{{input.*}}` | Job input payload | Execution runtime | Yes | `{{input.user_id}}` → `"u_abc"` |
| `{{config.*}}` | Endpoint's referenced config | Execution runtime | No (cached 60s) | `{{config.api_base_url}}` → `"https://api.myapp.com"` |
| `{{secret.*}}` | Encrypted secret store | Execution runtime | No (cached 300s) | `{{secret.email_api_key}}` → resolved at runtime, never exposed |
| `{{execution.*}}` | Execution metadata | Execution runtime | Yes | `{{execution.idempotency_key}}` → `"order-1234-welcome"` |

---

## Single-variable replacement vs string interpolation

The template engine distinguishes between two cases:

### Single-variable replacement (preserves type)

When a template variable is the **entire** string value, the resolved value preserves its native JSON type:

```json
{
  "amount": "{{input.amount}}",
  "active": "{{input.is_active}}"
}
```

If `input.amount` is `42` (integer) and `input.is_active` is `true` (boolean):

```json
{
  "amount": 42,
  "active": true
}
```

### String interpolation (always string)

When template variables are embedded in surrounding text, the result is always a string:

```json
{
  "url": "{{config.api_base_url}}/emails/welcome",
  "authorization": "Bearer {{secret.email_api_key}}"
}
```

Results in:

```json
{
  "url": "https://api.myapp.com/emails/welcome",
  "authorization": "Bearer sk-your-api-key"
}
```

:::tip
Use single-variable replacement when you need the native JSON type preserved (e.g. numbers, booleans, nested objects). Use string interpolation when building composite strings like URLs or authorization headers.
:::

---

## Recursive JSON walker

The template engine walks the entire JSON tree recursively. It processes:

- **Object values** — every string value in every key is checked for templates
- **Array elements** — every string element in every array is checked
- **Nested structures** — objects within objects, arrays within objects, etc.

```json
{
  "body_template": {
    "user": {
      "id": "{{input.user_id}}",
      "name": "{{input.user_name}}"
    },
    "orders": [
      "{{input.order_id_1}}",
      "{{input.order_id_2}}"
    ],
    "metadata": {
      "source": "invokr",
      "key": "{{secret.signing_key}}"
    }
  }
}
```

Every `{{...}}` occurrence at any depth is resolved.

---

## Fail-fast on unresolvable variables

If a template variable cannot be resolved (e.g. the config key doesn't exist, the secret was deleted, the input field is missing), the execution fails immediately:

```json
{
  "error": {
    "code": "TEMPLATE_RESOLUTION_FAILED",
    "message": "Template variable '{{secret.email_api_key}}' could not be resolved: secret not found",
    "request_id": "req_9a8b..."
  }
}
```

:::warning
Template resolution failures do **not** trigger retries. Since the same templates would fail identically on every attempt, retrying would only waste resources. The execution is marked as `FAILED` (or `RETRYING` → `FAILED` if this was not the first attempt, though this scenario is unlikely since template resolution happens before dispatch).
:::

---

## Examples of template usage in endpoint specs

### URL with config template

```json
{
  "url": "{{config.api_base_url}}/emails/welcome"
}
```

### Headers with secret template

```json
{
  "headers": {
    "Authorization": "Bearer {{secret.email_api_key}}",
    "Content-Type": "application/json"
  }
}
```

### Body template with input and config

```json
{
  "body_template": {
    "order_id": "{{input.order_id}}",
    "user_id": "{{input.user_id}}",
    "sender": "{{config.sender}}"
  }
}
```

### Kafka key and value templates

```json
{
  "key_template": "{{input.order_id}}",
  "value_template": {
    "event_type": "{{input.event_type}}",
    "order_id": "{{input.order_id}}",
    "amount": "{{input.amount}}"
  }
}
```

### Redis Stream fields template

```json
{
  "fields_template": {
    "user_id": "{{input.user_id}}",
    "title": "{{input.title}}",
    "body": "{{input.body}}"
  }
}
```

---

## Template resolution order

The worker resolves templates in a specific order:

1. **`{{config.*}}`** — resolved from the endpoint's referenced config (cached 60s)
2. **`{{secret.*}}`** — resolved from the encrypted secret store (cached 300s)
3. **`{{input.*}}`** — resolved from the execution's input payload
4. **`{{execution.*}}`** — resolved from execution metadata

With this order, config and secret values are available before input resolution, in case input templates reference them (uncommon, but possible).

---

## Auto-injected idempotency header

For HTTP dispatches, the worker automatically injects an `x-invokr-idempotency-key` header containing the execution's idempotency key. This allows downstream services to deduplicate retries safely:

- For `IMMEDIATE` and `DELAYED` jobs: the client-provided `idempotency_key`
- For `CRON` jobs: the system-generated key `cron_{job_id}_{epoch_ms}`

:::info
If you already set a header named `x-invokr-idempotency-key` (case-insensitive) in your endpoint's `headers`, the worker respects your value and does not override it.
:::

---

## Fallback behavior

If an endpoint's HTTP spec has no `body_template` and no `body` field, the worker injects the job's `input` object as the JSON request body. You can then fire jobs with arbitrary input without pre-defining a body template, which suits generic webhook-style endpoints.

---

## See also

- [Configs](./configs) — the `{{config.*}}` namespace
- [Secrets](./secrets) — the `{{secret.*}}` namespace
- [Payload Specs](./payload-specs) — input validation for `{{input.*}}`
- [Endpoints](./endpoints) — where templates are used
- [HTTP Endpoints Guide](../guides/http-endpoints) — template usage in HTTP endpoints
