---
id: secrets
title: Secrets
title_meta: Secrets API
---

# Secrets

Secrets are encrypted variables available in endpoint spec templates via the `{{secret.*}}` namespace. Unlike configs, secrets are **write-only** — the plaintext value is never returned in API responses. Secrets are encrypted at rest using AES-256-GCM and decrypted in memory only at execution time.

Use secrets for API keys, passwords, tokens, and other credentials that endpoints need to authenticate with downstream services.

## Authentication and headers

All secret endpoints require:

```
Authorization: Bearer <api_key>
X-Org-Id: <org_id>
X-Workspace-Id: <workspace_id>
```

:::warning
Secret endpoints are tenant-scoped. Requests without `X-Org-Id` and `X-Workspace-Id` headers will be rejected.
:::

## Encryption

### At-rest encryption

Secrets are encrypted using AES-256-GCM before being stored in the database. The encryption key is provided via `INVOKR_ENCRYPTION_KEY` (a 64-character hex string representing a 32-byte key).

The encryption process:
1. A 12-byte random nonce is generated for each encryption
2. The plaintext is encrypted using AES-256-GCM with the nonce and key
3. The nonce is prepended to the ciphertext
4. The combined `nonce || ciphertext || tag` is stored as `BYTEA` in the `secrets` table

### In-memory decryption

The worker decrypts secrets at execution time when resolving `{{secret.*}}` templates. Decrypted values are held in memory and cached for a configurable TTL (default: 300 seconds, controlled by `INVOKR_SECRET_CACHE_TTL_SEC`).

After the cache TTL expires, the decrypted value is evicted and the next request triggers a fresh decrypt from the database.

:::danger
**Never use the default encryption key in production.** The default `INVOKR_ENCRYPTION_KEY` is 64 zeros (`0000...0000`), which provides no security. Always set `INVOKR_ENCRYPTION_KEY` to a strong, random 32-byte key. If the key is rotated, existing secrets encrypted with the old key cannot be decrypted.
:::

## Write-only design

Secrets are designed to be **write-only** — the plaintext `value` is accepted on create and update, but never returned in any API response. All secret responses use the `SecretResponse` struct, which omits the `value` and `encrypted_value` fields entirely:

| Returned | Not returned |
|----------|-------------|
| `name` | `value` (plaintext input) |
| `created_at` | `encrypted_value` (BYTEA stored in DB) |
| `updated_at` | |

In practice:
- `POST /v1/secrets` accepts `value` in the request body but does not return it in the response
- `GET /v1/secrets` and `GET /v1/secrets/{name}` return only metadata
- `PUT /v1/secrets/{name}` accepts a new `value` but does not return it

## Template usage

Secrets are referenced in endpoint specs via `{{secret.*}}`:

```json
{
  "spec": {
    "url": "{{config.api_base_url}}/emails/welcome",
    "headers": {
      "Authorization": "Bearer {{secret.email_api_key}}"
    },
    "body_template": {
      "sender": "{{config.sender}}"
    }
  }
}
```

The `{{secret.email_api_key}}` template is resolved at execution time — the worker decrypts the secret value and substitutes it into the spec. The decrypted value is never logged or persisted.

See [Template resolution](../../core-concepts/templates) for full details.

## Fields

### Request fields (create/update)

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Unique secret name within the workspace |
| `value` | string | Plaintext secret value to encrypt and store (never returned in responses) |

### Response fields (all operations)

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Secret name |
| `created_at` | string (ISO 8601) | Creation timestamp |
| `updated_at` | string (ISO 8601) | Last update timestamp |

:::note
The `value` and `encrypted_value` fields are never present in API responses. Responses contain only metadata (`name`, `created_at`, `updated_at`).
:::

## Endpoints

### Create secret

```
POST /v1/secrets
```

Creates a new secret. The plaintext `value` is encrypted with AES-256-GCM before storage. Returns `409 Conflict` if a secret with the same name already exists.

**Request body:**

```json
{
  "name": "email_api_key",
  "value": "sk-your-api-key-here"
}
```

**Example:**

```bash
curl -X POST http://localhost:8080/v1/secrets \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..." \
  -H "Content-Type: application/json" \
  -d '{
    "name": "email_api_key",
    "value": "sk-your-api-key-here"
  }'
```

**Response (`201 Created`):**

```json
{
  "data": {
    "name": "email_api_key",
    "created_at": "2026-06-20T10:30:00Z",
    "updated_at": "2026-06-20T10:30:00Z"
  }
}
```

:::note
Notice that the `value` field is absent from the response. The plaintext value is encrypted and stored, but never returned.
:::

**Error responses:**

| Status | Error | Description |
|--------|-------|-------------|
| `409` | `Conflict` | Secret with name already exists |
| `500` | `Internal` | Encryption failed (check `INVOKR_ENCRYPTION_KEY` is valid) |

---

### List secrets

```
GET /v1/secrets?limit={limit}&cursor={cursor}
```

Returns secret metadata with cursor-based pagination. **Values are never included.**

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `limit` | integer | 50 | Maximum number of items to return (max 100) |
| `cursor` | string | *(none)* | Pagination cursor from the previous response |

**Example:**

```bash
curl "http://localhost:8080/v1/secrets?limit=10" \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..."
```

**Response (`200 OK`):**

```json
{
  "data": [
    {
      "name": "email_api_key",
      "created_at": "2026-06-20T10:30:00Z",
      "updated_at": "2026-06-20T10:30:00Z"
    },
    {
      "name": "webhook_signing_secret",
      "created_at": "2026-06-20T10:35:00Z",
      "updated_at": "2026-06-20T10:35:00Z"
    }
  ],
  "cursor": "d2Vic2l0ZQ=="
}
```

When `cursor` is present in the response, there are more items available. When `cursor` is `null`, all items have been returned.

:::info
The list response contains only metadata. There is no way to retrieve secret plaintext values via the API — they can only be written, not read.
:::

---

### Get secret

```
GET /v1/secrets/{name}
```

Returns metadata for a single secret. **The value is never included.**

**Example:**

```bash
curl http://localhost:8080/v1/secrets/email_api_key \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..."
```

**Response (`200 OK`):**

```json
{
  "data": {
    "name": "email_api_key",
    "created_at": "2026-06-20T10:30:00Z",
    "updated_at": "2026-06-20T10:30:00Z"
  }
}
```

**Error responses:**

| Status | Error | Description |
|--------|-------|-------------|
| `404` | `SecretNotFound` | Secret not found |

---

### Update secret

```
PUT /v1/secrets/{name}
```

Updates an existing secret's value. The new plaintext `value` is encrypted before storage. **The value is never returned in the response.**

**Request body:**

```json
{
  "value": "sk-new-rotated-api-key"
}
```

**Example:**

```bash
curl -X PUT http://localhost:8080/v1/secrets/email_api_key \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..." \
  -H "Content-Type: application/json" \
  -d '{
    "value": "sk-new-rotated-api-key"
  }'
```

**Response (`200 OK`):**

```json
{
  "data": {
    "name": "email_api_key",
    "created_at": "2026-06-20T10:30:00Z",
    "updated_at": "2026-06-20T11:45:00Z"
  }
}
```

**Error responses:**

| Status | Error | Description |
|--------|-------|-------------|
| `404` | `SecretNotFound` | Secret not found |
| `500` | `Internal` | Encryption failed |

:::tip
Secret value updates take effect after the worker's cache TTL expires (`INVOKR_SECRET_CACHE_TTL_SEC`, default 300 seconds). To force an immediate update, restart the worker.
:::

---

### Delete secret

```
DELETE /v1/secrets/{name}
```

Deletes a secret. Returns `409 Conflict` if any endpoints reference this secret (checked via `has_dependent_endpoints`, which scans endpoint spec templates for `{{secret.{name}}}` references).

**Example:**

```bash
curl -X DELETE http://localhost:8080/v1/secrets/email_api_key \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: org_3f8a2b1c-..." \
  -H "X-Workspace-Id: ws_5e2c8d9f-..."
```

**Response (`204 No Content`):**

No response body.

**Error responses:**

| Status | Error | Description |
|--------|-------|-------------|
| `404` | `SecretNotFound` | Secret not found |
| `409` | `Conflict` | Secret is referenced by endpoints — remove or update the endpoint's spec templates first |

:::warning
You cannot delete a secret that is referenced by any endpoint. The `has_dependent_endpoints` check scans all endpoint spec templates (including `url`, `headers`, and `body_template` fields) for `{{secret.{name}}}` references. First update or delete the dependent endpoints, then delete the secret.
:::

## Security considerations

| Aspect | Implementation |
|--------|----------------|
| **Encryption algorithm** | AES-256-GCM (authenticated encryption) |
| **Key** | 32-byte key from `INVOKR_ENCRYPTION_KEY` (hex string) |
| **Nonce** | 12-byte random nonce per encryption, prepended to ciphertext |
| **Storage** | `encrypted_value` column (BYTEA) in the `secrets` table |
| **API responses** | `SecretResponse` struct — never includes `value` or `encrypted_value` |
| **In-memory cache** | Decrypted values cached for `INVOKR_SECRET_CACHE_TTL_SEC` (default 300s) |
| **KMS integration** | `INVOKR_ENCRYPTION_KEY` itself can be KMS-encrypted (see [KMS](../../deployment/kms)) |

:::tip
For defense in depth, enable [AWS KMS integration](../../deployment/kms) to encrypt `INVOKR_ENCRYPTION_KEY` itself at rest. The encryption key is then never stored in plaintext in the environment; it's decrypted from KMS ciphertext at startup.
:::

## See also

- [Configs](./configs) — static variables referenced via `{{config.*}}`
- [Organizations](./organizations) — top-level tenant entity
- [Workspaces](./workspaces) — workspace creation and schema provisioning
- [Template resolution](../../core-concepts/templates) — how `{{secret.*}}` templates are resolved
- [AWS KMS Integration](../../deployment/kms) — encrypting `INVOKR_ENCRYPTION_KEY` via KMS
- [Environment Variables](../../configuration/environment-variables) — `INVOKR_ENCRYPTION_KEY`, `INVOKR_SECRET_CACHE_TTL_SEC`
