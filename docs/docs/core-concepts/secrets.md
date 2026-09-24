---
id: secrets
title: Secrets
---

# Secrets

Secrets are sensitive values (API keys, credentials, tokens) that are encrypted at rest and never exposed in API responses. They are referenced by endpoints via `{{secret.*}}` templates and resolved at execution time by the worker.

---

## What are secrets?

A secret is a named sensitive value stored encrypted in the database. Secrets let you reference credentials in endpoint specs without hardcoding them:

- **Encrypted at rest** using AES-256-GCM
- **Write-only** — the `value` is never returned in any API response
- **Cached in memory** by the worker (decrypted, 300s TTL)
- **Referenced via templates** — `{{secret.api_key}}` in endpoint specs

---

## AES-256-GCM encryption

Secrets are encrypted using AES-256-GCM (Galois/Counter Mode), which provides both confidentiality and authenticity:

| Property | Value |
|----------|-------|
| Algorithm | AES-256-GCM |
| Key source | `INVOKR_ENCRYPTION_KEY` environment variable (hex string, 32 bytes = 64 hex chars) |
| Nonce | Random 12-byte nonce per encryption |
| Storage format | Nonce prepended to ciphertext: `nonce (12 bytes) || ciphertext || tag (16 bytes)` |
| Decryption | Worker decrypts on cache miss, stores decrypted value in memory |

:::danger
**In production, always set `INVOKR_ENCRYPTION_KEY` to a strong, random 32-byte key.** The default all-zeros key (`0000...0000`) provides no security. If the key is rotated, existing secrets encrypted with the old key cannot be decrypted.
:::

:::info
When KMS is enabled (`INVOKR_KMS_ENABLED=true`), `INVOKR_ENCRYPTION_KEY` itself is expected to be a base64-encoded KMS-encrypted ciphertext, transparently decrypted at startup. See [AWS KMS Integration](../deployment/kms).
:::

---

## Creating a secret

```bash
curl -X POST http://localhost:8080/v1/secrets \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "email_api_key",
    "value": "sk-your-api-key"
  }'
```

Response (`201 Created` — **value is never returned**):

```json
{
  "name": "email_api_key",
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

:::warning
The `value` field is **write-only**. It is accepted on `POST` and `PUT` requests but is never included in any API response. Not in `GET`, not in `LIST`, not in creation/update responses. Only metadata (`name`, `created_at`, `updated_at`) is returned.
:::

---

## Secret value never returned in API responses

All secret API responses return only metadata — never the value:

| Endpoint | Returns |
|----------|---------|
| `POST /v1/secrets` | `{ "name", "created_at", "updated_at" }` |
| `GET /v1/secrets` | List of `{ "name", "created_at", "updated_at" }` (names only, no values) |
| `GET /v1/secrets/{name}` | `{ "name", "created_at", "updated_at" }` (metadata only) |
| `PUT /v1/secrets/{name}` | `{ "name", "created_at", "updated_at" }` (after rotation) |

---

## Secret caching

Secrets are cached in the worker process to avoid repeated decryption and database lookups:

| Property | Value |
|----------|-------|
| Cache implementation | `DashMap<String, (DecryptedSecret, Instant)>` |
| TTL | 300 seconds (5 minutes, configurable via `INVOKR_SECRET_CACHE_TTL_SEC`) |
| Storage | Decrypted value stored in memory |
| Eviction | Lazy — entries are refreshed on next access after TTL expires |

:::info
Secret rotation (via `PUT /v1/secrets/{name}`) takes effect for future executions after the cache TTL expires. Due to caching, there may be a delay of up to `INVOKR_SECRET_CACHE_TTL_SEC` (default: 300s / 5 minutes) before rotated secrets are visible to the worker.
:::

---

## Template resolution with `{{secret.*}}`

Secrets are referenced in endpoint specs using `{{secret.*}}` templates. The worker resolves these at execution time by decrypting the secret value from the cache (or database on cache miss):

```json
{
  "name": "send-welcome-email",
  "type": "HTTP",
  "spec": {
    "url": "{{config.api_base_url}}/emails/welcome",
    "headers": {
      "Authorization": "Bearer {{secret.email_api_key}}"
    }
  }
}
```

| Template | Secret name | Resolved value |
|----------|------------|----------------|
| `{{secret.email_api_key}}` | `email_api_key` | Decrypted value (e.g. `"sk-your-api-key"`) |
| `{{secret.stripe_secret}}` | `stripe_secret` | Decrypted value |

:::tip
Secrets can appear in URL strings, header values, and body template fields — anywhere templates are supported. The template engine walks the entire JSON tree. If a secret is unresolvable (e.g. the secret was deleted), the execution fails immediately with `TEMPLATE_RESOLUTION_FAILED`. There is no retry, since it would fail the same way.
:::

---

## How endpoints reference secrets in spec templates

Endpoints reference secrets implicitly through `{{secret.*}}` templates in their spec. Unlike payload specs and configs, secrets are **not** referenced by a named field on the endpoint — they are referenced inline within template strings:

```json
{
  "name": "send-welcome-email",
  "type": "HTTP",
  "payload_spec": "order-input",
  "config": "email-service",
  "spec": {
    "headers": {
      "Authorization": "Bearer {{secret.email_api_key}}"
    },
    "body_template": {
      "api_key": "{{secret.email_api_key}}"
    }
  }
}
```

:::warning
Because secrets are referenced inline in templates, deleting a secret that is used by an endpoint does **not** return `409 CONFLICT` at deletion time. Instead, executions that reference the deleted secret will fail at runtime with `TEMPLATE_RESOLUTION_FAILED`. Always ensure secrets are removed from endpoint specs before deleting them.
:::

---

## Managing secrets

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/secrets` | Create a secret (write-only) |
| `GET` | `/v1/secrets` | List all secrets (names only, no values) |
| `GET` | `/v1/secrets/{name}` | Get secret metadata (no value) |
| `PUT` | `/v1/secrets/{name}` | Rotate / update a secret value |
| `DELETE` | `/v1/secrets/{name}` | Delete (fails if endpoints reference it) |

:::note
Secret rotation via `PUT` updates the encrypted value in the database. The old value is overwritten. Due to caching, executions may use the old value for up to `INVOKR_SECRET_CACHE_TTL_SEC` (default: 300s) after rotation.
:::

---

## See also

- [Endpoints](./endpoints) — how secrets are used in specs
- [Configs](./configs) — non-sensitive counterpart to secrets
- [Templates](./templates) — the template resolution engine
- [Environment Variables](../configuration/environment-variables) — `INVOKR_ENCRYPTION_KEY`, `INVOKR_SECRET_CACHE_TTL_SEC`
- [AWS KMS Integration](../deployment/kms) — encrypting sensitive variables with KMS
- [The Three-Step Workflow](./overview) — where secrets fit in the model
