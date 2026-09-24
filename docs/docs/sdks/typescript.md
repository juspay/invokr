---
id: typescript
title: TypeScript SDK
---

# TypeScript SDK

The TypeScript SDK is generated from Smithy IDL models and provides a fully typed client for the Invokr REST API. It's published as the `invokr-sdk` npm package.

## Installation

The TypeScript SDK is generated and compiled via the justfile:

```bash
# Generate from Smithy models and compile the npm package
just build-sdk

# Install CLI dependencies (links to the built SDK)
just cli-install
```

This produces the `invokr-sdk` package in `smithy/build/smithy/source/typescript-client-codegen/`, which is then linked into the `cli/` project.

## Client setup

Create an `InvokrServiceClient` with the API endpoint and bearer token:

```typescript
import { InvokrServiceClient, CreateJobCommand } from "invokr-sdk";

const client = new InvokrServiceClient({
  endpoint: "http://localhost:8080",
  token: { token: "dev-api-key" },
});
```

| Parameter | Description | Default |
|-----------|-------------|---------|
| `endpoint` | Invokr API base URL | — |
| `token.token` | Bearer token for authentication | — |

For tenant-scoped operations, pass `org_id` and `workspace_id` with each command:

```typescript
const tenant = {
  org_id: process.env.INVOKR_ORG_ID!,
  workspace_id: process.env.INVOKR_WORKSPACE_ID!,
};
```

## Creating a job

```typescript
import { InvokrServiceClient, CreateJobCommand } from "invokr-sdk";

const client = new InvokrServiceClient({
  endpoint: "http://localhost:8080",
  token: { token: "dev-api-key" },
});

const response = await client.send(
  new CreateJobCommand({
    ...tenant,
    endpoint: "send-welcome-email",
    trigger: "IMMEDIATE",
    idempotency_key: "order-1234-welcome",
    input: { order_id: "order-1234", user_id: "u_abc" },
  }),
);

console.log(response.data!.job_id);
```

### Response shape

For `IMMEDIATE` and `DELAYED` triggers, the response includes an `execution` object:

```typescript
console.log(response.data!.job_id);           // "job_8f3a..."
console.log(response.data!.execution!.execution_id);  // "exec_2b7c..."
console.log(response.data!.execution!.status);         // "QUEUED"
```

For `CRON` triggers, the response includes scheduling metadata:

```typescript
console.log(response.data!.job_id);        // "job_c72f..."
console.log(response.data!.cron);         // "0 9 * * MON"
console.log(response.data!.next_run_at);  // "2026-03-16T09:00:00+05:30"
```

## Other operations

### Create an endpoint

```typescript
import { CreateEndpointCommand } from "invokr-sdk";

const endpointResp = await client.send(
  new CreateEndpointCommand({
    ...tenant,
    name: "send-welcome-email",
    endpoint_type: "HTTP",
    spec: {
      method: "POST",
      url: "https://api.myapp.com/emails/welcome",
      headers: {
        "Content-Type": "application/json",
      },
    },
    retry_policy: {
      max_attempts: 3,
      backoff: "exponential",
      initial_delay_ms: 500,
      max_delay_ms: 5000,
    },
  }),
);
```

### Create a payload spec

```typescript
import { CreatePayloadSpecCommand } from "invokr-sdk";

await client.send(
  new CreatePayloadSpecCommand({
    ...tenant,
    name: "order-input",
    schema: {
      type: "object",
      properties: {
        order_id: { type: "string" },
        user_id: { type: "string" },
      },
      required: ["order_id"],
    },
  }),
);
```

### Get an execution

```typescript
import { GetExecutionCommand } from "invokr-sdk";

const execResp = await client.send(
  new GetExecutionCommand({
    ...tenant,
    execution_id: "exec_2b7c...",
  }),
);

console.log(execResp.data!.status);        // "SUCCESS"
console.log(execResp.data!.attempt_count); // 1
console.log(execResp.data!.duration_ms);   // 340
```

### Cancel a job

```typescript
import { CancelJobCommand } from "invokr-sdk";

await client.send(
  new CancelJobCommand({
    ...tenant,
    job_id: "job_c72f...",
  }),
);
```

### List executions for a job

```typescript
import { ListJobExecutionsCommand } from "invokr-sdk";

const execsResp = await client.send(
  new ListJobExecutionsCommand({
    ...tenant,
    job_id: "job_8f3a...",
  }),
);

const executions = execsResp.data ?? [];
for (const exec of executions) {
  console.log(`${exec.execution_id}: ${exec.status}`);
}
```

### List execution attempts

```typescript
import { ListExecutionAttemptsCommand } from "invokr-sdk";

const attemptsResp = await client.send(
  new ListExecutionAttemptsCommand({
    ...tenant,
    execution_id: "exec_2b7c...",
  }),
);

const attempts = attemptsResp.data ?? [];
for (const attempt of attempts) {
  console.log(`#${attempt.attempt_number} — ${attempt.status} (${attempt.duration_ms}ms)`);
}
```

## CLI test scripts

The `cli/src/` directory contains test scripts that exercise the full Invokr lifecycle using the TypeScript SDK:

| Script | Just Recipe | Description |
|--------|------------|-------------|
| `test-immediate.ts` | `just test-immediate` | Create an endpoint, fire an IMMEDIATE job, poll until terminal state |
| `test-delayed.ts` | `just test-delayed` | Test delayed job execution with `run_at` |
| `test-cron.ts` | `just test-cron` | Test CRON job scheduling and execution |
| `load-test.ts` | `just load-test 50` | Create 50 jobs of each type and track completion |
| `test-internal-guards.ts` | — | Verify API guards reject user jobs targeting INTERNAL endpoints |

### Running tests

```bash
# Prerequisites: all services running
just dev

# Set tenant environment variables
export INVOKR_ORG_ID="<your_org_id>"
export INVOKR_WORKSPACE_ID="<your_workspace_id>"

# Run individual tests
just test-immediate    # Test immediate job execution
just test-delayed      # Test delayed job execution
just test-cron         # Test CRON job execution

# Load testing
just load-test 50      # Create 50 jobs of each type
just load-test-nw 50   # Fire-and-forget (no polling)
```

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_URL` | `http://localhost:8080` | Invokr API base URL |
| `MOCK_URL` | `http://localhost:9999` | Mock server base URL |
| `INVOKR_API_KEY` | `dev-api-key` | Bearer token for API authentication |
| `INVOKR_ORG_ID` | *(required)* | Organization ID for tenant routing |
| `INVOKR_WORKSPACE_ID` | *(required)* | Workspace ID for tenant routing |

### Test script example (test-immediate.ts)

The test scripts follow a common pattern:

1. Create an HTTP endpoint pointing to the mock server
2. Create a job targeting that endpoint
3. Poll `ListJobExecutions` until the execution reaches a terminal state (`SUCCESS`, `FAILED`, `CANCELLED`)
4. Print execution details, attempts, and job status
5. Clean up (cancel job, delete endpoint)

```typescript
const TERMINAL_STATUSES = new Set(["SUCCESS", "FAILED", "CANCELLED"]);

while (Date.now() - startTime < POLL_TIMEOUT_MS) {
  const execsResp = await client.send(
    new ListJobExecutionsCommand({ ...tenant, job_id: jobId }),
  );

  const executions = execsResp.data ?? [];
  if (executions.length > 0) {
    const exec = executions[0];
    if (TERMINAL_STATUSES.has(exec.status!)) {
      // Fetch full execution details
      const fullExec = await client.send(
        new GetExecutionCommand({ ...tenant, execution_id: exec.execution_id! }),
      );
      break;
    }
  }
  await sleep(POLL_INTERVAL_MS);
}
```

## Error handling

The SDK throws errors with metadata for non-success responses:

```typescript
try {
  await client.send(new CreateJobCommand({ ... }));
} catch (err: any) {
  console.error(`${err.name}: ${err.message}`);
  if (err.$metadata) {
    console.error(`HTTP ${err.$metadata.httpStatusCode}`);
  }
}
```

:::tip
The SDK uses the AWS Smithy TypeScript runtime, so it follows the same command/response pattern as the AWS SDK for JavaScript. If you have used `aws-sdk` v3, the API here works the same way.
:::

## Related pages

- [SDK Overview](./overview) — All SDKs and the Smithy codegen pipeline
- [Rust SDK](./rust) — Generated Rust SDK for Rust consumers
- [Haskell SDK](./haskell) — Generated Haskell SDK
