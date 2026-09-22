---
id: rust
title: Rust SDK
---

# Rust SDK

The Rust SDK is generated from Smithy IDL models via `smithy-rs` codegen. It provides a fully typed, async client for the Invokr REST API with a fluent-builder pattern.

## Location and Status

The generated Rust SDK lives at `crates/client/` and is published as the `invokr_sdk` crate.

```toml
[dependencies]
invokr_sdk = { git = "https://github.com/juspay/invokr.git" }
```

### Excluded from Workspace Build

The `invokr_sdk` crate is **excluded** from the Invokr workspace (`Cargo.toml` → `[workspace] exclude`) for two reasons:

1. **Different MSRV**: The SDK targets Rust 1.82, while the server crates target a different minimum version
2. **Heavy AWS runtime stack**: The SDK pulls in the AWS smithy runtime, which the server crates don't need

### Committed to Repository

Despite being generated, the `crates/client/` directory is committed to the repository. This means downstream Rust consumers (e.g. other services within the organization) can depend on it via a Cargo `git` dep without needing the Smithy CLI, JVM, or Maven to build:

```toml
[dependencies]
invokr_sdk = { git = "https://github.com/juspay/invokr.git", tag = "v0.1.0" }
```

:::warning
**DO NOT EDIT FILES IN `crates/client/`.** Every file here is overwritten on each regeneration — any hand edits will be lost silently. See `crates/client/README.md` for details.
:::

## Regeneration

After changing anything under `smithy/model/`:

```bash
# Validate model, regenerate, sync to crates/client/
just smithy-build

# Review the resulting diff
git diff -- crates/client/

# Commit model + generated SDK in the same PR
git add smithy/ crates/client/
git commit
```

## Runnable Example

A complete, working program lives at [`examples/sdk-mode/`](https://github.com/juspay/invokr/tree/main/examples/sdk-mode)
— it creates an endpoint, fires an IMMEDIATE job, polls the execution to a
terminal state and prints every attempt. It is the Rust counterpart of
`cli/src/test-immediate.ts` and `haskell-example/`, so the three SDKs can be
read side by side.

```bash
just dev                            # API, worker, mock server
./scripts/setup-dev-tenant.sh       # writes org/workspace ids to .env
just example-sdk-mode
```

## Client Setup

```rust
use invokr_sdk::config::{Config, Token};
use invokr_sdk::Client;

let client = Client::from_conf(
    Config::builder()
        .behavior_version_latest()
        .endpoint_url("http://localhost:8080")
        .bearer_token(Token::new("dev-api-key", None))
        .build(),
);
```

:::warning
`behavior_version_latest()` is required. Without it the SDK **panics at client
construction** — "A behavior major version must be set" — and nothing catches
it at compile time. The alternative is enabling the crate's
`behavior-version-latest` feature.
:::

## Usage Example

```rust
use aws_smithy_types::Document;
use invokr_sdk::types::TriggerTypeEnum;
use std::collections::HashMap;

// `input` and `spec` are smithy `Document`s, not `serde_json::Value`, and
// there is no `json!` macro for them.
let input = Document::Object(HashMap::from([
    ("order_id".to_string(), Document::String("order-1234".into())),
    ("user_id".to_string(), Document::String("u_abc".into())),
]));

let response = client
    .create_job()
    .org_id("org_abc")
    .workspace_id("ws_def")
    .endpoint("send-welcome-email")
    .trigger(TriggerTypeEnum::Immediate)
    .idempotency_key("order-1234-welcome")
    .input(input)
    .send()
    .await?;

println!("Job ID: {}", response.data.job_id);
```

:::note
`ListJobExecutions` returns a leaner `ExecutionResource` than `GetExecution`:
`duration_ms`, `worker_id`, `endpoint` and `idempotency_key` are left unset on
the list route. Poll the list for a terminal status, then call `GetExecution`
for the full record.
:::

## Fluent-Builder Pattern

The Rust SDK uses the fluent-builder pattern (consistent with the AWS SDK for Rust). Each operation has:

- A builder type (e.g., `CreateJobFluentBuilder`) that chains setter methods
- An input type (e.g., `CreateJobInput`) with `builder()` for manual construction
- An output type (e.g., `CreateJobOutput`) containing the response data

```rust
// Fluent builder (preferred)
let response = client.create_job()
    .org_id("org_abc")
    .workspace_id("ws_def")
    .endpoint("send-welcome-email")
    .trigger(TriggerType::Immediate)
    .send()
    .await?;

// Manual input construction
let input = CreateJobInput::builder()
    .org_id("org_abc")
    .workspace_id("ws_def")
    .endpoint("send-welcome-email")
    .trigger(TriggerType::Immediate)
    .build()?;

let response = client.create_job().set_input(input).send().await?;
```

## Key Types

| Type | Description |
|------|-------------|
| `Client` | Main SDK client, holds HTTP connection and configuration |
| `Config` | Client configuration (endpoint URL, auth token, retry config) |
| `Error` | Unified error type for all SDK operations |
| `operation::*` | Operation modules (e.g., `create_job`, `cancel_job`, `get_execution`) |
| `types::*` | Data types (e.g., `JobResource`, `ExecutionResource`, `EndpointResource`) |
| `types::TriggerType` | Enum: `Immediate`, `Delayed`, `Cron` |
| `types::ExecutionStatus` | Enum: `Pending`, `Queued`, `Running`, `Retrying`, `Success`, `Failed`, `Cancelled` |
| `types::EndpointType` | Enum: `Http`, `Kafka`, `RedisStream` |

## Codegen Configuration

The `smithy-build.json` configures the Rust codegen:

```json
{
  "rust-client-codegen": {
    "runtimeConfig": { "version": "1.2.5" },
    "codegen": {
      "addMessageToErrors": true,
      "renameErrors": true,
      "enableNewSmithyRuntime": "orchestrator"
    },
    "service": "com.invokr#InvokrService",
    "module": "invokr_sdk",
    "moduleVersion": "0.1.0",
    "minimumSupportedRustVersion": "1.82.0",
    "moduleDescription": "Rust SDK for Invokr job scheduling service"
  }
}
```

| Config | Value | Description |
|--------|-------|-------------|
| `runtimeConfig.version` | `1.2.5` | AWS smithy runtime version |
| `codegen.addMessageToErrors` | `true` | Include error messages in error types |
| `codegen.renameErrors` | `true` | Rename Smithy errors to idiomatic Rust names |
| `codegen.enableNewSmithyRuntime` | `orchestrator` | Use the new smithy orchestrator runtime |
| `service` | `com.invokr#InvokrService` | Smithy service shape |
| `module` | `invokr_sdk` | Cargo crate name |
| `minimumSupportedRustVersion` | `1.82.0` | MSRV for the generated crate |

## Related Pages

- [SDK Overview](./overview) — All SDKs and the Smithy codegen pipeline
- [TypeScript SDK](./typescript) — TypeScript SDK for JS/TS consumers
- [Haskell SDK](./haskell) — Haskell SDK for Haskell consumers
