---
id: pagination
title: Pagination
---

# Pagination

All list endpoints in Invokr use **cursor-based pagination**. It returns stable results even as new records are inserted concurrently, unlike offset-based pagination, which can skip or duplicate records when data changes between page fetches.

## Query parameters

| Parameter | Type | Default | Description |
|-----------|------|:-------:|-------------|
| `limit` | integer | `50` | Number of items per page. Range: `1`–`200`. |
| `cursor` | string | *(none)* | Opaque pagination cursor from the previous response. Base64-encoded. |

:::info
The cursor is an opaque, base64-encoded string containing the last seen `created_at` timestamp and record ID. You should treat it as an opaque token — do not parse or construct it manually.
:::

## Response shape

All list endpoints return a `PaginatedResponse<T>`:

```json
{
  "data": [ ... ],
  "cursor": "eyJjcmVhdGVkX2F0IjoiMjAyNi0wMy0xNVQxMDowMDowMFoiLCJpZCI6ImpvYl..."}
}
```

| Field | Type | Description |
|-------|------|-------------|
| `data` | array | Array of items for the current page. |
| `cursor` | string \| null | Cursor for the next page. When `null` or absent, there are no more pages. |

## When to stop iterating

- If `cursor` is `null` or absent from the response, you have reached the last page.
- If `cursor` is present, pass it as the `cursor` query parameter on the next request.

## Endpoints that support pagination

All `GET` list endpoints support cursor-based pagination:

| Endpoint | Description |
|----------|-------------|
| `GET /v1/orgs` | List organizations |
| `GET /v1/orgs/{org_id}/workspaces` | List workspaces |
| `GET /v1/payload-specs` | List payload specs |
| `GET /v1/configs` | List configs |
| `GET /v1/secrets` | List secrets (names only) |
| `GET /v1/endpoints` | List endpoints |
| `GET /v1/jobs` | List jobs (with filters) |
| `GET /v1/jobs/{job_id}/executions` | List executions for a job |
| `GET /v1/jobs/{job_id}/versions` | List version history |
| `GET /v1/executions/{id}/attempts` | List attempts for an execution |
| `GET /v1/executions/{id}/logs` | List execution logs |

## Example: listing jobs with limit and cursor

### First page

```bash
curl "http://localhost:8080/v1/jobs?limit=10" \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

Response:

```json
{
  "data": [
    {
      "job_id": "job_a1b2...",
      "endpoint": "send-welcome-email",
      "trigger": "IMMEDIATE",
      "status": "ACTIVE",
      "version": 1,
      "created_at": "2026-03-15T10:05:00Z"
    },
    {
      "job_id": "job_c3d4...",
      "endpoint": "publish-order-event",
      "trigger": "CRON",
      "status": "ACTIVE",
      "version": 1,
      "created_at": "2026-03-15T09:30:00Z"
    }
  ],
  "cursor": "eyJjcmVhdGVkX2F0IjoiMjAyNi0wMy0xNVQwOTozMDowMFoiLCJpZCI6ImpvYl9jM2Q0Li4uIn0="
}
```

### Next page

Pass the cursor from the previous response:

```bash
curl "http://localhost:8080/v1/jobs?limit=10&cursor=eyJjcmVhdGVkX2F0IjoiMjAyNi0wMy0xNS0xNVQwOTozMDowMFoiLCJpZCI6ImpvYl9jM2Q0Li4uIn0=" \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

When the response contains `"cursor": null`, there are no more pages.

## Example: iterating through all pages

```bash
#!/usr/bin/env bash
set -e

BASE_URL="http://localhost:8080"
AUTH="Authorization: Bearer dev-api-key"
TENANT="X-Org-Id: <org_id> -H X-Workspace-Id: <workspace_id>"
CURSOR=""
PAGE=1

while true; do
  if [ -z "$CURSOR" ]; then
    URL="$BASE_URL/v1/jobs?limit=50"
  else
    URL="$BASE_URL/v1/jobs?limit=50&cursor=$CURSOR"
  fi

  RESPONSE=$(curl -s "$URL" \
    -H "$AUTH" \
    -H "X-Org-Id: <org_id>" \
    -H "X-Workspace-Id: <workspace_id>")

  COUNT=$(echo "$RESPONSE" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('data',[])))")
  echo "Page $PAGE: $COUNT items"

  CURSOR=$(echo "$RESPONSE" | python3 -c "import sys,json; c=json.load(sys.stdin).get('cursor'); print(c if c else '')")

  if [ -z "$CURSOR" ]; then
    echo "No more pages."
    break
  fi

  PAGE=$((PAGE + 1))
done
```

## Example: iterating with the TypeScript SDK

```typescript
import { InvokrServiceClient, ListJobsCommand } from "invokr-sdk";

const client = new InvokrServiceClient({
  endpoint: "http://localhost:8080",
  token: { token: "dev-api-key" },
});

async function listAllJobs() {
  const allJobs = [];
  let cursor: string | undefined;

  do {
    const response = await client.send(
      new ListJobsCommand({
        limit: 200,
        cursor,
      })
    );

    allJobs.push(...response.data.data);
    cursor = response.data.cursor ?? undefined;
  } while (cursor);

  console.log(`Total jobs: ${allJobs.length}`);
  return allJobs;
}
```

## Job list filters

The `GET /v1/jobs` endpoint supports additional query parameters for filtering alongside pagination:

| Parameter | Type | Description |
|-----------|------|-------------|
| `endpoint` | string | Filter by endpoint name |
| `trigger` | string | Filter: `IMMEDIATE`, `DELAYED`, `CRON` |
| `status` | string | Filter: `ACTIVE`, `RETIRED` |
| `from` | ISO 8601 | Start of time range |
| `to` | ISO 8601 | End of time range |

Example with filters:

```bash
curl "http://localhost:8080/v1/jobs?trigger=CRON&status=ACTIVE&limit=20" \
  -H "Authorization: Bearer dev-api-key" \
  -H "X-Org-Id: <org_id>" \
  -H "X-Workspace-Id: <workspace_id>"
```

## Why cursor-based pagination?

| Approach | Behavior under concurrent inserts | Consistency |
|----------|----------------------------------|-------------|
| **Offset-based** (`?offset=50`) | New inserts between page fetches cause items to shift, leading to skipped or duplicated records | Inconsistent |
| **Cursor-based** (`?cursor=...`) | Cursor is based on the last item's sort key. New inserts before the cursor do not affect subsequent pages | Consistent |

Invokr sorts records by `created_at DESC` (and by ID as a tiebreaker). The cursor encodes the last seen `created_at` + `id`, ensuring stable iteration even as new records are created.

:::tip
Always use the maximum `limit` (200) when bulk-exporting data to minimize the number of round trips. Each page request is a separate HTTP call.
:::

## See also

- [CRON jobs](./cron-jobs) — listing executions for scheduled jobs
- [Job versioning](./versioning) — viewing version history with pagination
- [HTTP endpoints](./http-endpoints) — listing endpoints
- [Monitoring](./monitoring) — using metrics instead of polling for status
