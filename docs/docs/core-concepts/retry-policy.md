---
id: retry-policy
title: Retry Policy
---

# Retry Policy

The retry policy defines how Invokr retries failed executions. It is configurable per endpoint and supports three backoff strategies with jitter to prevent thundering herd effects.

---

## Overview

When a dispatch attempt fails (e.g. HTTP 503, Kafka broker error, Redis timeout), the worker consults the endpoint's retry policy to determine:

1. **Whether to retry** — if `attempt_count < max_attempts`, the execution is set to `RETRYING`
2. **How long to wait** — the backoff delay is computed based on the strategy and attempt number
3. **When to give up** — if `attempt_count >= max_attempts`, the execution is set to `FAILED`

:::note
The retry policy is defined on the **endpoint**, not the job. All jobs fired against an endpoint share the same retry policy. If you need different retry behavior, create separate endpoints.
:::

---

## RetryPolicy fields

| Field | Type | Default | Description |
|-------|------|:-------:|-------------|
| `max_attempts` | integer | `1` | Total attempts including the first. `1` = no retries (fire once, fail on error). |
| `backoff` | string | `exponential` | Backoff strategy: `fixed`, `linear`, or `exponential`. |
| `initial_delay_ms` | integer | `1000` | Delay before the first retry, in milliseconds. |
| `max_delay_ms` | integer | `60000` | Upper bound on backoff delay, in milliseconds. |

:::info
`max_attempts` counts the **total** number of attempts, including the initial one. So `max_attempts: 3` means 1 initial attempt + 2 retries. `max_attempts: 1` means no retries — the execution fails immediately if the first attempt fails.
:::

---

## Backoff strategies

Three backoff strategies are supported:

| Strategy | Formula | Use case |
|----------|---------|----------|
| `fixed` | `initial_delay_ms` | Consistent retry interval. Good for transient failures that resolve quickly. |
| `linear` | `initial_delay_ms * attempt` | Gradually increasing delay. Good for systems under gradually increasing load. |
| `exponential` | `initial_delay_ms * 2^(attempt-1)` | Back off quickly under pressure. Good for overloaded systems where rapid retries would worsen the situation. |

### Example calculations

With `initial_delay_ms: 1000` and `max_delay_ms: 60000`:

| Attempt | `fixed` | `linear` | `exponential` |
|---------|---------|----------|---------------|
| 1 (initial) | — | — | — |
| 2 (retry 1) | 1000ms | 1000ms | 1000ms |
| 3 (retry 2) | 1000ms | 2000ms | 2000ms |
| 4 (retry 3) | 1000ms | 3000ms | 4000ms |
| 5 (retry 4) | 1000ms | 4000ms | 8000ms |
| 6 (retry 5) | 1000ms | 5000ms | 16000ms |
| 7 (retry 6) | 1000ms | 6000ms | 32000ms |
| 8 (retry 7) | 1000ms | 7000ms | 60000ms (capped) |
| 9 (retry 8) | 1000ms | 8000ms | 60000ms (capped) |

:::note
The attempt number used in the formula is the retry attempt number (1 for the first retry, 2 for the second, etc.), not the total attempt count.
:::

---

## Jitter

All backoff strategies apply **±25% jitter** to prevent thundering herd effects:

```rust
fn compute_backoff(policy: &RetryPolicy, attempt: i32) -> i64 {
    let delay = match policy.backoff {
        Fixed => policy.initial_delay_ms,
        Linear => policy.initial_delay_ms * attempt as i64,
        Exponential => policy.initial_delay_ms * 2_i64.pow((attempt - 1) as u32),
    };
    // Add ±25% jitter to prevent thundering herd
    let jitter = rand::thread_rng().gen_range(-(delay/4)..=(delay/4));
    (delay + jitter).clamp(0, policy.max_delay_ms)
}
```

For example, with `initial_delay_ms: 1000` and `exponential` backoff:
- Retry 1: base = 1000ms, jitter range = [750ms, 1250ms], actual = random value in that range
- Retry 2: base = 2000ms, jitter range = [1500ms, 2500ms], actual = random value in that range

:::tip
Jitter is essential when multiple workers are retrying failed executions simultaneously. Without jitter, all workers would retry at the same intervals, creating synchronized load spikes on your downstream services.
:::

---

## Capping at `max_delay_ms`

The computed backoff delay (after jitter) is clamped to `max_delay_ms`:

```
actual_delay = (base_delay + jitter).clamp(0, max_delay_ms)
```

This ensures that even with exponential backoff, the delay never exceeds the configured maximum. Once the base delay exceeds `max_delay_ms`, all subsequent retries wait exactly `max_delay_ms` (±jitter).

---

## Example retry policy in endpoint creation

```json
{
  "name": "send-welcome-email",
  "type": "HTTP",
  "payload_spec": "order-input",
  "config": "email-service",
  "spec": {
    "url": "{{config.api_base_url}}/emails/welcome",
    "method": "POST",
    "timeout_ms": 5000,
    "expected_status_codes": [200, 201, 202, 204]
  },
  "retry_policy": {
    "max_attempts": 3,
    "backoff": "exponential",
    "initial_delay_ms": 1000,
    "max_delay_ms": 30000
  }
}
```

This policy means:
- **3 total attempts** (1 initial + 2 retries)
- **Exponential backoff** starting at 1000ms
- Retry 1: ~1000ms (±250ms jitter)
- Retry 2: ~2000ms (±500ms jitter)
- Maximum delay capped at 30000ms

---

## Retry behavior by transport

### HTTP

An HTTP attempt fails when:
- The response status code is **not** in `expected_status_codes` (error type: `HTTP_ERROR`)
- The request times out (error type: `TIMEOUT`)
- The connection fails (error type: `CONNECTION_ERROR`)

All three failure types trigger the retry policy.

### Kafka

A Kafka attempt fails when:
- The broker returns an error (error type: `BROKER_ERROR`)
- The produce operation times out (error type: `TIMEOUT`)

### Redis Stream

A Redis Stream attempt fails when:
- The connection fails (error type: `CONNECTION_ERROR`)
- The operation times out (error type: `TIMEOUT`)
- The stream operation fails (error type: `STREAM_ERROR`)

---

## Execution retry flow

When an attempt fails and retries remain:

1. The worker records the attempt (status: `FAILED`, with error details)
2. The worker computes the backoff delay
3. The execution is updated: `status = RETRYING`, `run_at = now() + backoff_delay`, `worker_id = NULL`
4. The worker releases the semaphore permit
5. On the next poll cycle, the worker picks up the `RETRYING` execution once `run_at <= now()`
6. A new attempt is made (attempt count incremented)

When all attempts are exhausted:

1. The worker records the final attempt (status: `FAILED`)
2. The execution is updated: `status = FAILED`, `completed_at = now()`

---

## Crashed worker recovery

There is no reclaimer, and nothing to wait out. The claim that sets an execution to `RUNNING`
is part of the same transaction that dispatches it, and that transaction only commits once the
attempt has been recorded. If the worker dies first, PostgreSQL rolls the whole thing back:

1. The execution reverts to the status it had before the claim — `QUEUED`, `PENDING` or `RETRYING` —
   with its previous `attempt_count` and no `worker_id`
2. The row is eligible for pickup immediately, so the next worker to poll (default: every 200ms)
   claims it
3. No attempt row is written for the lost try, because that write rolled back too

So a crashed worker costs you one poll interval, not a timeout. An execution can only be observed
as `RUNNING` while some worker's transaction is actually holding it.

:::warning
The flip side: if the worker died *after* the target received the request, that delivery happened
and Invokr has no record of it — the retry will deliver it again. This is why delivery is
at-least-once and why receivers should de-duplicate on `x-invokr-idempotency-key`. See
[Delivery Guarantees](../architecture/exactly-once).
:::

:::info
Graceful shutdown (`SIGINT`/`SIGTERM`) does not roll anything back: the worker stops polling and
waits up to `INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC` (default 30s) for in-flight executions to commit.
:::

---

## See also

- [Executions](./executions) — the execution lifecycle and state machine
- [Endpoints](./endpoints) — where retry policies are defined
- [Environment Variables](../configuration/environment-variables) — scheduler configuration
- [HTTP Endpoints Guide](../guides/http-endpoints) — HTTP-specific retry behavior
