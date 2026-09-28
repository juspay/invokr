# Long-running jobs: polling + callback semantics

**Status:** Draft (post-brainstorming)
**Date:** 2026-06-11
**Scope:** Worker, API server, dashboard, DB schema. HTTP endpoint type only.

## Problem

Kronos today treats every dispatch as synchronous: the worker `POST`s to an
endpoint, classifies the response as SUCCESS / FAILURE / retry-needed in one
shot, and writes the terminal status. Many real destinations are themselves
asynchronous — they return `202 Accepted` immediately and continue work in the
background. Kronos has no way to model this, so a long-running destination has
to be wrapped in a custom adapter that itself polls and posts back, or Kronos
has to pretend the work is done the moment dispatch returns.

We want first-class long-running semantics so that:

1. An endpoint can be marked as async — Kronos accepts a "work pending"
   response from the destination and waits for the real terminal signal.
2. Kronos can learn about completion by **polling** the destination on a
   schedule, or via a **callback** the destination POSTs back into Kronos, or
   both at once. Whichever arrives first wins.
3. The existing attempt/retry semantics stay clean: polling is *not* a
   dispatch attempt; only the original send is. A failed long-running job
   triggers the same RetryPolicy that a failed sync dispatch would.
4. Long-running executions are still bounded — destinations that go silent
   are eventually marked FAILED with a TIMEOUT.
5. Cancellation and observability work the same as for any other execution.

This document is the design. The implementation plan is its own document.

## Non-goals

- Async semantics for KAFKA / REDIS_STREAM endpoints. Long-running over a
  message-broker dispatch is a different shape of problem; HTTP first.
- Heartbeat / deadline-extension API.
- HMAC signing of callback request bodies. Bearer api_key + tenant-scoped URL
  is the auth surface for v1.
- Per-callback custom paths/headers.
- Authenticated DELETE on cancel beyond reusing the endpoint's own header
  block.

## Summary of decisions

| Topic | Decision |
|---|---|
| Async detection | Endpoint declares `async.status_codes` (e.g. `[202]`). Disjoint from `expected_status_codes`. |
| Poll URL | Captured from the initial response's `Location` header. Relative URLs resolved against the resolved initial URL per RFC 3986. |
| Poll-response interpretation | Status code: `poll.success_statuses` / `pending_statuses` / `failure_statuses` (all disjoint). Anything else → transient error. |
| Cadence | Honour `Retry-After` (seconds or HTTP-date); fall back to spec backoff (`initial_delay_ms`, `max_delay_ms`, `backoff` shape). |
| Bounds | `max_wait_ms` AND `max_polls`. Per-job overrides allowed within sane bounds. Default 1h / 1000 polls when unspecified. Either bound trips → FAILED with `TIMEOUT`. |
| Polls vs attempts | Polls live outside `attempt_count`. Transient poll errors keep polling. Destination-signalled terminal failure triggers existing retry path (re-dispatch from initial). |
| Callback auth | Bearer api_key + tenant in URL path. No per-execution token. |
| Mode coexistence | Endpoint may enable polling, callback, or both. Whichever finalizes first wins. |
| Cancellation | Local CANCELLED + best-effort bare `DELETE` on stored `poll_url` with resolved endpoint headers. Fire-and-forget, 5s timeout. |
| Missing `Location` on async response | FAILED with `MISSING_POLL_URL`. Non-retryable. |
| State machine | Two new statuses: `WAITING` (parked, due to poll later) and `POLLING` (claim is mid-poll). |

## State machine

```
PENDING ──► QUEUED ──► RUNNING ──► SUCCESS
                                ├─► FAILED
                                ├─► RETRYING ──► (run_at) ──► RUNNING
                                └─► WAITING

WAITING ──(run_at elapses)──► POLLING ──► SUCCESS
                                       ├─► FAILED
                                       ├─► RETRYING ──► (run_at) ──► RUNNING
                                       └─► WAITING

CANCELLED is terminal. Reachable from PENDING, QUEUED, WAITING.
RUNNING and POLLING are not directly cancellable — caller retries cancel
when the execution lands back in WAITING / RETRYING / QUEUED.
```

All terminal transitions on long-running executions filter on
`status IN ('WAITING','POLLING')` so a callback, a poll completion, and a
cancel can race safely — exactly one row update wins, others see zero rows
affected and log a benign "already finalized" trace.

## Data model

### `ExecutionStatus` (Rust enum + DB `CHECK` constraint)

Existing variants unchanged. Add:

- `WAITING` — parked between polls.
- `POLLING` — claim is currently mid-poll.

### `executions` table additions (additive migration)

| Column | Type | Notes |
|---|---|---|
| `poll_url` | `TEXT NULL` | Captured from `Location` on the async response. Cleared on re-dispatch. |
| `poll_count` | `INT NOT NULL DEFAULT 0` | Bound enforcement for `max_polls`. Bumped only on poll claims. |
| `polling_started_at` | `TIMESTAMPTZ NULL` | Set when first entering WAITING for the current attempt. Reset on re-dispatch. |
| `polling_deadline` | `TIMESTAMPTZ NULL` | `polling_started_at + max_wait_ms` precomputed so claim SQL doesn't need the endpoint spec. |
| `max_wait_ms` | `BIGINT NULL` | Effective value (endpoint default + per-job override) snapshotted on WAITING entry. |
| `max_polls` | `INT NULL` | Same shape. |

`started_at` is set once at the first dispatch and **preserved** across
WAITING ↔ POLLING cycles for that attempt. On re-dispatch (poll terminal
failure), `started_at` is reset along with the four polling columns.

### `polls` table (new; mirrors `attempts`)

```sql
CREATE TABLE polls (
  execution_id    TEXT        NOT NULL REFERENCES executions(execution_id),
  poll_number     INT         NOT NULL,
  polled_at       TIMESTAMPTZ NOT NULL,
  duration_ms     BIGINT,
  status_code     INT,
  retry_after_ms  BIGINT,
  classification  TEXT        NOT NULL,
  error           JSONB,
  PRIMARY KEY (execution_id, poll_number)
);
```

`classification` is one of `SUCCESS`, `PENDING`, `TERMINAL_FAILURE`,
`TRANSIENT_ERROR`. `status_code` is NULL on transport-level failures (where
`error` carries the cause).

### `jobs` table additions (additive migration)

| Column | Type | Notes |
|---|---|---|
| `async_max_wait_ms` | `BIGINT NULL` | Effective value (override-or-endpoint-default) resolved at job creation. NULL when the endpoint isn't async. |
| `async_max_polls` | `INT NULL` | Same shape. |

Resolved once at job creation so:

- pg_cron CRON ticks copy the same value onto each new execution without
  re-reading the endpoint spec each tick;
- endpoint edits don't retroactively change bounds on already-created jobs;
- per-execution snapshot on `executions.*` ensures even mid-flight changes
  don't affect the in-flight execution.

### Index changes

The existing `(status, run_at)` index covers the dispatch hot path. Extending
the claim WHERE clause to include `'WAITING'` uses the same index — no new
index needed for hot path. The `polls` table primary key
`(execution_id, poll_number)` covers per-execution listing.

## Endpoint spec extensions (HTTP)

Long-running config lives inside the existing `spec` JSON under a new `async`
block. Presence of `async` is the opt-in.

```json
{
  "url": "https://api.example.com/jobs",
  "method": "POST",
  "headers": { "Authorization": "Bearer {{secret.api_token}}" },
  "expected_status_codes": [200, 201],

  "async": {
    "status_codes": [202],

    "poll": {
      "success_statuses": [200],
      "pending_statuses": [202],
      "failure_statuses": [410, 422],
      "initial_delay_ms": 1000,
      "max_delay_ms": 60000,
      "backoff": "exponential"
    },

    "callback": {
      "enabled": true
    },

    "max_wait_ms": 3600000,
    "max_polls": 1000
  }
}
```

### Semantics

- **`async.status_codes`** — initial-dispatch status codes that mean "work
  pending". Must be disjoint from `expected_status_codes`. A response in this
  set transitions RUNNING → WAITING; a `Location` header is required.
- **`async.poll`** — presence enables polling. The three status sets must be
  disjoint. Any poll status code outside all three is `TRANSIENT_ERROR`. So
  is any transport-level error (connect, read, timeout).
- **`async.callback`** — presence enables callback. `enabled: true` is the
  only field today. Structured as an object to leave room for future per-
  endpoint callback options without breaking the spec.
- **At least one of `poll` or `callback`** must be present when `async` is.
- **`max_wait_ms` and `max_polls`** apply to the total wait (both modes).
  Either bound trips → FAILED with `error.type = "TIMEOUT"`. Endpoint
  defaults are 3_600_000 (1h) and 1000 respectively when omitted from the
  spec.

### Headers, auth, URL resolution

- Polling GETs reuse the top-level `headers` block. Same auth as initial
  dispatch. No `poll.headers` for v1.
- A relative `Location` is resolved against the resolved initial-dispatch
  URL using RFC 3986.
- Missing `Location` on a response in `async.status_codes` → FAILED with
  `error.type = "MISSING_POLL_URL"`. Non-retryable.

### Validation (enforced at endpoint create/update)

- `async.status_codes ∩ expected_status_codes = ∅`.
- `poll.success_statuses ∩ pending_statuses ∩ failure_statuses` pairwise empty.
- At least one of `poll` / `callback` present when `async` is.
- All numeric bounds positive; `max_wait_ms ≤ 30 days`,
  `max_polls ≤ 100_000`.

## Job-level overrides

```json
POST /v1/jobs
{
  "trigger": "IMMEDIATE",
  "endpoint": "my-async-endpoint",
  "input": { ... },
  "async_overrides": {
    "max_wait_ms": 7200000,
    "max_polls": 2000
  }
}
```

Both fields inside `async_overrides` are individually optional. Whatever is
omitted falls back to the endpoint's `async.max_wait_ms` / `async.max_polls`.

### Validation

- `async_overrides` present but endpoint has no `async` block → `400
  INVALID_OVERRIDES_NO_ASYNC`.
- Bounds: `1 ≤ max_wait_ms ≤ 30 days`, `1 ≤ max_polls ≤ 100_000`.

### Effect on `GET` responses

`GET /v1/jobs/{id}` and `GET /v1/executions/{id}` expose the effective
`async_max_wait_ms` / `async_max_polls`. Helps debug "why did my job time
out at 1h when I configured 2h" — users look at the execution row, not the
possibly-later-edited endpoint.

## Worker pipeline

### Extended `claim()` SQL

```sql
UPDATE {executions} e
SET
  status = CASE WHEN e.status = 'WAITING' THEN 'POLLING' ELSE 'RUNNING' END,
  worker_id = $1,
  started_at = CASE WHEN e.status = 'WAITING' THEN e.started_at ELSE now() END,
  attempt_count = CASE WHEN e.status = 'WAITING' THEN e.attempt_count
                       ELSE e.attempt_count + 1 END,
  poll_count = CASE WHEN e.status = 'WAITING' THEN e.poll_count + 1
                    ELSE e.poll_count END
WHERE e.execution_id = (
    SELECT execution_id FROM {executions}
    WHERE status IN ('QUEUED','RETRYING','PENDING','WAITING')
      AND run_at <= now()
    ORDER BY run_at ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
RETURNING execution_id, job_id, endpoint, endpoint_type, input,
          attempt_count, max_attempts,
          status AS claim_status,
          poll_url, poll_count,
          max_wait_ms, max_polls,
          polling_started_at, polling_deadline
```

Invariants:

- `attempt_count` bumps **only** on dispatch claims.
- `poll_count` bumps **only** on poll claims.
- `started_at` is set once and preserved across WAITING ↔ POLLING.
- One UPDATE per claim — no second round-trip.

### Pipeline branching

```rust
match exec.claim_status.as_str() {
    "RUNNING" => process_dispatch(ctx, db, ..., &exec).await,
    "POLLING" => process_poll(ctx, db, ..., &exec).await,
    _ => { /* unreachable; defensive complete_failed */ }
}
```

### `process_dispatch` — existing path + async detection

After the HTTP call returns, before existing SUCCESS / FAILURE branches:

| Initial response | New outcome |
|---|---|
| status ∈ `async.status_codes`, `Location` present | Resolve Location → absolute URL. Atomic transition: RUNNING → WAITING. Set `poll_url`, `polling_started_at = now()`, `polling_deadline = now() + max_wait_ms`, `run_at = min(polling_deadline, now() + initial_delay_or_retry_after)`. Records an `attempts` row with classification `WAITING`. |
| status ∈ `async.status_codes`, no `Location` | `complete_failed_non_retryable("MISSING_POLL_URL")`. |
| status ∈ `expected_status_codes` | existing SUCCESS path. |
| else | existing FAILURE path → RETRYING / FAILED. |

### `process_poll` — new path

1. **Bound check before any network call.** If `poll_count > max_polls` OR
   `now() > polling_deadline` → `complete_failed_timeout()`, no GET issued.
2. **Load endpoint, resolve templated headers only.** Body/URL templates
   aren't relevant for the poll GET.
3. **GET `poll_url`** with resolved headers and the endpoint's `timeout_ms`.
4. **Record a `polls` row** in the same transaction.
5. **Classify and act:**

| Poll response | Action | Status transition |
|---|---|---|
| status ∈ `poll.success_statuses` | `complete_success(output = body)` | POLLING → SUCCESS |
| status ∈ `poll.failure_statuses` | `retry_from_poll()` — clears polling columns; consults `attempt_count` vs `max_attempts`; sets RETRYING with backoff or FAILED if budget exhausted | POLLING → RETRYING or FAILED |
| status ∈ `poll.pending_statuses` | `next_run_at = min(polling_deadline, now() + retry_after_or_spec_backoff)`. `transition_back_to_waiting(next_run_at)` | POLLING → WAITING |
| Transport error or status ∉ any set | Same as pending. Recorded as `TRANSIENT_ERROR`. No `attempt_count` change. | POLLING → WAITING |

### Cap on `run_at` for WAITING rows

When transitioning to WAITING, `next_run_at` is **capped at
`polling_deadline`**. Guarantees a Retry-After-doomed execution is still
picked up at the deadline and finalized as TIMEOUT.

### New DB helpers (in `db::executions`)

- `transition_to_waiting(execution_id, poll_url, polling_started_at, polling_deadline, next_run_at)`
- `transition_back_to_waiting(execution_id, next_run_at)`
- `retry_from_poll(execution_id, backoff_ms)` — clears polling columns + uses
  existing budget check
- `complete_failed_timeout(execution_id)` — sets FAILED with
  `error = {type: "TIMEOUT", ...}`

All use atomic `UPDATE ... WHERE status = 'POLLING'` (or `'RUNNING'` for the
WAITING transition).

### Code locations

- `crates/worker/src/pipeline.rs` gains a top-level branch and a new
  sibling file `poll.rs` for poll-specific logic.
- `crates/worker/src/dispatcher/http.rs` is **unchanged** — still does one
  HTTP call and returns a `DispatchResult`. Async detection lives in the
  pipeline because it's about *what to do with the result*, not how to
  send the request.
- `crates/common/src/db/executions.rs` gains the four helpers above plus an
  extended `ClaimedExecution` struct.
- `crates/common/src/db/polls.rs` — new module mirroring
  `db::attempts`.
- `crates/common/src/db/jobs.rs` — write paths set the new `async_max_*`
  columns; read paths surface them.

### What doesn't change

- The poller loop in `crates/worker/src/poller.rs` — same `claim_and_process`
  flow.
- The semaphore-based concurrency control.
- The multi-tenant schema iteration.
- The `attempts` table semantics.
- The HTTP dispatcher.

## Callback API + templates

### Routes

```
POST /v1/callbacks/{org_id}/{workspace_id}/executions/{execution_id}/complete
POST /v1/callbacks/{org_id}/{workspace_id}/executions/{execution_id}/fail
```

Dedicated `/v1/callbacks/...` namespace with tenant in the URL rather than
the existing tenant-header convention because:

- The URL is handed verbatim to an external system; one URL is a simpler
  contract than "URL plus two headers".
- It localises the trust model: this namespace is documented as a callback
  receiver. The rest of the API stays as-is.
- Avoids needing a global `execution_id → (org, workspace)` lookup table in
  `public` — keeps schema-per-tenant isolation clean.

Both require `Authorization: Bearer <api_key>` (same key as the rest of the
API).

### Request bodies

```http
POST /v1/callbacks/{org}/{ws}/executions/{id}/complete
Authorization: Bearer <api_key>
Content-Type: application/json

{ "output": <any JSON> }
```

```http
POST /v1/callbacks/{org}/{ws}/executions/{id}/fail
Authorization: Bearer <api_key>
Content-Type: application/json

{ "error": <any JSON> }
```

`output` / `error` are stored verbatim on the execution row.

### Atomic finalization

```sql
-- /complete
UPDATE {executions}
SET status = 'SUCCESS', output = $2, completed_at = now(),
    duration_ms = (EXTRACT(EPOCH FROM (now() - started_at)) * 1000)::BIGINT
WHERE execution_id = $1
  AND status IN ('WAITING','POLLING')
```

`/fail` calls the same `retry_from_poll(execution_id, backoff_ms)` helper
the poll path uses. Symmetry: how the failure was signalled doesn't affect
whether it retries.

### Response codes

| Outcome | HTTP | Body |
|---|---|---|
| Row updated, finalized | `200` | finalized execution row |
| Status already terminal (SUCCESS / FAILED / CANCELLED) | `409` | `{ "code": "ALREADY_TERMINAL", "current_status": "..." }` |
| Status is PENDING / QUEUED / RUNNING / RETRYING | `409` | `{ "code": "NOT_YET_WAITING" }` |
| Row doesn't exist in this tenant | `404` | — |
| Org/workspace doesn't exist or api_key wrong scope | `403` | — |

The `409 ALREADY_TERMINAL` response is idempotent-friendly: destinations
retrying callbacks on network blips don't see false errors.

### New template variables

Resolved at initial-dispatch template-resolution time, alongside existing
`{{execution.*}}` variables:

| Template | Value |
|---|---|
| `{{execution.callback_url}}` | Full URL to `/complete`. Alias for `callback_url_success`. |
| `{{execution.callback_url_success}}` | Full URL to `/complete`. |
| `{{execution.callback_url_failure}}` | Full URL to `/fail`. |
| `{{execution.org_id}}` | Tenant org_id. |
| `{{execution.workspace_id}}` | Tenant workspace_id. |

Base URL comes from existing `TE_API_BASE_URL` / `TE_PATH_PREFIX` config.

Example body template:

```json
"body_template": {
  "task_input": "{{input.payload}}",
  "on_success": "{{execution.callback_url_success}}",
  "on_failure": "{{execution.callback_url_failure}}"
}
```

## Cancellation

### Extended `db::executions::cancel`

```sql
UPDATE {executions}
SET status = 'CANCELLED', completed_at = now()
WHERE execution_id = $1
  AND status IN ('PENDING','QUEUED','WAITING')
RETURNING *, status AS previous_status, poll_url
```

`WAITING` joins `PENDING` and `QUEUED` as cancellable. `RUNNING` and
`POLLING` stay non-cancellable — matches the existing pattern. In-flight
HTTP runs to completion; caller retries cancel a second later when the row
is back in WAITING.

### Best-effort DELETE on `poll_url`

When the cancel UPDATE returns a row where `previous_status = 'WAITING'` and
`poll_url IS NOT NULL`, the API handler:

1. Loads the endpoint to get the `headers` block.
2. Resolves any `{{secret.*}}` tokens in headers (direct fetch+decrypt; no
   cache; cancels are rare).
3. Spawns a fire-and-forget `tokio::spawn` issuing `DELETE poll_url` with
   resolved headers and a 5-second timeout.
4. Logs success/failure via `tracing` and a row in `execution_logs`.
5. The cancel API response returns `200` immediately, regardless of the
   DELETE outcome.

The `load_secrets` / `load_single_secret` helpers currently in
`crates/worker/src/pipeline.rs` are lifted to `crates/common/src/secrets.rs`
(cache-optional). Worker keeps its cached variant; API uses the no-cache
variant.

## Race surface

All terminal transitions on long-running executions use
`WHERE status IN ('WAITING','POLLING')`. Possible races:

| Racer | Wins if status was | Effect on losers |
|---|---|---|
| Worker poll → SUCCESS | WAITING (claim) → POLLING → SUCCESS | callback `/complete` sees `ALREADY_TERMINAL` → 409 |
| Worker poll → terminal failure | same | callback `/fail` sees `ALREADY_TERMINAL` → 409 |
| Callback `/complete` | WAITING or POLLING | worker's terminal UPDATE no-ops; logs "execution already finalized" |
| Cancel | PENDING / QUEUED / WAITING | if status was POLLING or terminal: 409 |

Zero double-finalization possible — every transition is a single atomic
UPDATE filtered on a closed set of source statuses.

## Observability

### Metrics (Prometheus, follows existing `kronos_*` naming)

| Metric | Type | Labels |
|---|---|---|
| `kronos_executions_waiting` | gauge | `schema`, `endpoint` |
| `kronos_executions_polling` | gauge | `schema`, `endpoint` |
| `kronos_polls_total` | counter | `schema`, `endpoint`, `classification` |
| `kronos_poll_duration_seconds` | histogram | `schema`, `endpoint` |
| `kronos_callbacks_received_total` | counter | `schema`, `endpoint`, `kind`, `result` |
| `kronos_long_running_completed_total` | counter | `schema`, `endpoint`, `terminator`, `status` |

`classification` ∈ `{SUCCESS, PENDING, TERMINAL_FAILURE, TRANSIENT_ERROR}`.
`kind` ∈ `{complete, fail}`.
`result` ∈ `{applied, already_terminal, not_yet_waiting, not_found}`.
`terminator` ∈ `{poll, callback, timeout, cancel}`.

### Execution logs

Worker emits to the existing `execution_logs` table at:

- `INFO  Entered WAITING; will poll {url} in {ms}ms`
- `INFO  Poll #N → {status_code} ({classification}); next poll in {ms}ms`
- `INFO  Poll #N → {status_code} success after {ms}ms total`
- `WARN  Poll #N → {status_code} terminal failure; will re-dispatch (attempt {n}/{max})`
- `WARN  Polling timeout: {wall_ms}ms ≥ max_wait_ms ({deadline})`
- `WARN  Poll budget exhausted: {poll_count} ≥ max_polls`
- `INFO  Callback received: complete`
- `INFO  Callback received: fail → re-dispatch`
- `INFO  Cancel during WAITING; sent DELETE to {url} → {result}`

### Dashboard

- **Execution detail page**: a new "Polls" panel beside the existing
  "Attempts" panel. Row per poll: number, time, response status,
  classification, duration, Retry-After.
- **Status filter / colours** for `WAITING` and `POLLING` in the executions
  list. Existing status pill component gets two new variants.

No new API routes needed for the dashboard — `GET /v1/executions/{id}`
returns the new columns plus a `polls` array.

## Testing strategy

### Unit (`cargo test` per crate)

- `db::executions::claim` with mixed `WAITING` / `QUEUED` rows; assert
  `attempt_count` / `poll_count` / `status` transitions.
- `process_poll` classification table: synthetic responses against every
  status-set bucket.
- Spec validation: malformed `async` blocks (overlapping status sets,
  missing required fields, both modes off, out-of-range overrides).
- URL resolution: relative `Location` against various initial URLs.
- `Retry-After` parsing: integer-seconds and HTTP-date forms.

### Integration (`mock-server` extensions)

New mock-server routes:

- `POST /async/start` — returns `202 Location: /async/status/{id}` and
  remembers the id in in-process state.
- `GET /async/status/{id}` — programmable per test: return `202` N times
  then `200`, or `410` immediately, or always `202`, etc.
- `DELETE /async/status/{id}` — flags the id as cancelled in mock-server
  state so tests can assert it was called.

Integration matrix (one test per row):

| Scenario | Expected terminal status | Side effect |
|---|---|---|
| 202 → poll returns 200 on first poll | SUCCESS | `polls` has 1 row |
| 202 → poll returns 202 ×3 then 200 | SUCCESS | `polls` has 4 rows; `attempt_count` = 1 |
| 202 → poll returns 500 then 200 | SUCCESS | `polls` has a `TRANSIENT_ERROR` row; `attempt_count` unchanged |
| 202 → poll returns 410 | RETRYING then RUNNING (re-dispatch) | `attempt_count` bumps; poll columns cleared |
| 202 with no `Location` | FAILED | `error.type = "MISSING_POLL_URL"`; no retry |
| 202 → poll always returns 202; `max_wait_ms` elapses | FAILED | `error.type = "TIMEOUT"` |
| 202 → poll always returns 202; `max_polls` reached before deadline | FAILED | same |
| 202 → callback `/complete` arrives | SUCCESS via callback | next scheduled poll no-ops |
| 202 → callback `/fail` arrives | RETRYING (re-dispatch) | matches poll terminal-failure semantics |
| Callback arrives twice (retry) | first 200, second `409 ALREADY_TERMINAL` | |
| Cancel during WAITING | CANCELLED | mock-server records the DELETE call |
| Cancel during POLLING (race) | 409 from cancel API; execution completes normally | |
| Per-job `max_wait_ms` override honoured | TIMEOUT at override boundary | |

### Multi-tenancy regression

One existing integration test extended to assert two tenants' WAITING /
POLLING rows don't bleed across schemas.

## Migration

Single additive migration. No data backfill needed because all new columns
are NULL-safe and the new statuses are unreachable from existing data
without the new code paths.

```sql
-- new statuses
ALTER TABLE {executions} DROP CONSTRAINT executions_status_check;
ALTER TABLE {executions} ADD CONSTRAINT executions_status_check
  CHECK (status IN ('PENDING','QUEUED','RUNNING','RETRYING',
                    'SUCCESS','FAILED','CANCELLED',
                    'WAITING','POLLING'));

-- executions long-running columns
ALTER TABLE {executions}
  ADD COLUMN poll_url            TEXT,
  ADD COLUMN poll_count          INT  NOT NULL DEFAULT 0,
  ADD COLUMN polling_started_at  TIMESTAMPTZ,
  ADD COLUMN polling_deadline    TIMESTAMPTZ,
  ADD COLUMN max_wait_ms         BIGINT,
  ADD COLUMN max_polls           INT;

-- jobs override columns
ALTER TABLE {jobs}
  ADD COLUMN async_max_wait_ms   BIGINT,
  ADD COLUMN async_max_polls     INT;

-- polls table
CREATE TABLE {polls} (
  execution_id    TEXT        NOT NULL REFERENCES {executions}(execution_id),
  poll_number     INT         NOT NULL,
  polled_at       TIMESTAMPTZ NOT NULL,
  duration_ms     BIGINT,
  status_code     INT,
  retry_after_ms  BIGINT,
  classification  TEXT        NOT NULL,
  error           JSONB,
  PRIMARY KEY (execution_id, poll_number)
);
```

The migration runs both against the workspace template
(`workspace_v1.sql`) so new tenants are provisioned with the new shape, and
against every existing tenant schema via the existing migration runner.

## Open questions

None at design time. All major semantic and structural choices have been
decided through the brainstorming session that produced this document.
Implementation-time decisions (exact metric label cardinality limits, mock-
server route shapes, validation error messages) are left for the
implementation plan and code review.
