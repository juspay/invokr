---
id: payload-specs
title: Payload Specs
---

# Payload Specs

A payload spec is a JSON Schema that defines the input contract for an endpoint. When an endpoint references a payload spec, every job's `input` is validated against the schema at creation time, before any execution is created. Invalid input never reaches your downstream services.

---

## What is a payload spec?

Payload specs serve as the type system for job input. They:

- Define the expected shape of job input (object properties, types, required fields)
- Are stored as JSON Schema documents in the database
- Are referenced by endpoints via the `payload_spec` field (by name)
- Validate job input **at creation time** — if validation fails, the API returns `422 INPUT_VALIDATION_FAILED` and no job or execution is created

:::info
Payload specs are optional. An endpoint without a `payload_spec` reference accepts any input. However, using payload specs is strongly recommended to catch input errors early and provide clear error messages to API consumers.
:::

---

## Creating a payload spec

```bash
curl -X POST http://localhost:8080/v1/payload-specs \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "order-input",
    "schema": {
      "type": "object",
      "properties": {
        "order_id": { "type": "string", "description": "The order identifier" },
        "user_id": { "type": "string", "description": "The user who placed the order" }
      },
      "required": ["order_id"]
    }
  }'
```

Response (`201 Created`):

```json
{
  "name": "order-input",
  "schema": {
    "type": "object",
    "properties": {
      "order_id": { "type": "string", "description": "The order identifier" },
      "user_id": { "type": "string", "description": "The user who placed the order" }
    },
    "required": ["order_id"]
  },
  "created_at": "2026-03-15T10:00:00Z",
  "updated_at": "2026-03-15T10:00:00Z"
}
```

---

## JSON Schema support

Invokr uses the `jsonschema` crate for validation. The following JSON Schema features are supported:

| Feature | Example |
|---------|---------|
| `type` | `"type": "object"`, `"type": "string"`, `"type": "integer"` |
| `properties` | Defines the expected properties of an object |
| `required` | `"required": ["order_id"]` — lists mandatory properties |
| `description` | `"description": "The order identifier"` — documentation |
| Additional JSON Schema keywords | `minimum`, `maximum`, `pattern`, `enum`, `items`, etc. |

:::tip
Keep your schemas focused on the fields your endpoint actually needs. Avoid overly permissive schemas (e.g. `additionalProperties: true` with no `properties`) as they provide no validation value. Use `required` to enforce mandatory fields and catch missing input early.
:::

---

## How payload specs validate job input

When a job is created (`POST /v1/jobs`), the validation flow is:

1. The API loads the endpoint definition
2. If the endpoint has a `payload_spec` reference, the payload spec is loaded
3. The job's `input` object is validated against the payload spec's JSON Schema
4. If validation passes, the job and execution are created in a transaction
5. If validation fails, the API returns `422 INPUT_VALIDATION_FAILED` with details:

```json
{
  "error": {
    "code": "INPUT_VALIDATION_FAILED",
    "message": "Input does not match schema: missing required field 'order_id'",
    "request_id": "req_9a8b..."
  }
}
```

:::note
For `CRON` jobs, the `input` is validated once at job creation time and then used for every tick. CRON job input must therefore be static; you can't vary it per execution.
:::

---

## How endpoints reference payload specs

Endpoints reference payload specs by name (not ID) via the `payload_spec` field:

```json
{
  "name": "send-welcome-email",
  "type": "HTTP",
  "payload_spec": "order-input",
  "config": "email-service",
  "spec": { ... }
}
```

The reference is validated at endpoint creation time. If the payload spec doesn't exist, the API returns `422 INVALID_PAYLOAD_SPEC_REF`.

:::warning
You cannot delete a payload spec that is referenced by an endpoint. The API returns `409 CONFLICT`. Remove the reference from all endpoints before deleting.
:::

---

## Managing payload specs

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/payload-specs` | Create a payload spec |
| `GET` | `/v1/payload-specs` | List all payload specs |
| `GET` | `/v1/payload-specs/{name}` | Get a payload spec |
| `PUT` | `/v1/payload-specs/{name}` | Update a payload spec |
| `DELETE` | `/v1/payload-specs/{name}` | Delete (fails if endpoints reference it) |

:::note
Updating a payload spec does not affect running executions. The updated schema applies to future job creations only.
:::

---

## See also

- [Endpoints](./endpoints) — how payload specs are referenced
- [Jobs](./jobs) — where input is validated
- [Templates](./templates) — how input is used in endpoint specs
- [The Three-Step Workflow](./overview) — where payload specs fit in the model
