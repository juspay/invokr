---
id: rust
title: Rust SDK
---

# Rust SDK

The Rust SDK is generated from Smithy IDL models via `smithy-rs` codegen. It provides a fully typed, async client for the Invokr REST API with a fluent-builder pattern.

## Location and status

The generated Rust SDK lives at `crates/client/` and is published as the `invokr_sdk` crate.

```toml
[dependencies]
invokr_sdk = { git = "https://github.com/juspay/invokr.git" }
```

### Excluded from workspace build

The `invokr_sdk` crate is **excluded** from the Invokr workspace (`Cargo.toml` → `[workspace] exclude`) for two reasons:

1. **Different MSRV**: The SDK targets Rust 1.82, while the server crates target a different minimum version
2. **Heavy AWS runtime stack**: The SDK pulls in the AWS smithy runtime, which the server crates don't need

### Committed to repository

Despite being generated, the `crates/client/` directory is committed to the repository. Downstream Rust consumers (e.g. other services within the organization) can then depend on it via a Cargo `git` dep without needing the Smithy CLI, JVM, or Maven to build:

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

## Client setup

```rust
use invokr_sdk::Client;
use invokr_sdk::config::Config;

// Build the client
let config = Config::builder()
    .endpoint_url("http://localhost:8080")
    .bearer_token("dev-api-key")
    .build();

let client = Client::from_conf(config);
```

## Usage example

```rust
use invokr_sdk::Client;
use invokr_sdk::types::TriggerType;
use invokr_sdk::operation::create_job::CreateJobInput;

let client = Client::from_conf(config);

let response = client.create_job()
    .org_id("org_abc")
    .workspace_id("ws_def")
    .endpoint("send-welcome-email")
    .trigger(TriggerType::Immediate)
    .idempotency_key("order-1234-welcome")
    .input(serde_json::json!({
        "order_id": "order-1234",
        "user_id": "u_abc"
    }))
    .send()
    .await?;

println!("Job ID: {}", response.data.job_id);
```

## Fluent-builder pattern

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

## Key types

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

## Codegen configuration

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

## Related pages

- [SDK Overview](./overview) — All SDKs and the Smithy codegen pipeline
- [TypeScript SDK](./typescript) — TypeScript SDK for JS/TS consumers
- [Haskell SDK](./haskell) — Haskell SDK for Haskell consumers
