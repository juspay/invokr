---
id: overview
title: SDK Overview
---

# SDK Overview

Invokr ships SDKs for several languages, and all of them are generated from the same Smithy IDL models. Because the clients come out of one definition, they stay in step with the API — and with each other — and nobody has to hand-write or hand-update client code.

## Smithy IDL models

All SDKs are generated from Smithy interface definition language (IDL) models located at `smithy/model/*.smithy`. Smithy is Amazon's open-source IDL for defining services and their data shapes.

The Smithy models define:

- **Service operations** (e.g., `CreateJob`, `CreateEndpoint`, `CancelJob`)
- **Input/output shapes** for each operation
- **Data types** (e.g., `JobResource`, `ExecutionResource`, `EndpointResource`)
- **Enums** (e.g., `TriggerType`, `ExecutionStatus`, `EndpointType`)
- **Error types** (e.g., `JobNotFound`, `InvalidCron`)

### smithy-build.json configuration

The `smithy/smithy-build.json` file configures code generation for three languages plus an OpenAPI spec:

```json
{
  "version": "1.0",
  "sources": ["model"],
  "maven": {
    "dependencies": [
      "software.amazon.smithy:smithy-model:1.55.0",
      "software.amazon.smithy:smithy-utils:1.55.0",
      "software.amazon.smithy:smithy-codegen-core:1.55.0",
      "software.amazon.smithy:smithy-aws-traits:1.55.0",
      "software.amazon.smithy:smithy-validation-model:1.55.0",
      "software.amazon.smithy:smithy-openapi:1.55.0",
      "software.amazon.smithy.typescript:smithy-aws-typescript-codegen:0.26.0",
      "in.juspay.smithy.haskell:client-codegen:0.0.4",
      "software.amazon.smithy.rust.codegen:codegen-client:0.1.0"
    ]
  },
  "plugins": {
    "typescript-client-codegen": {
      "package": "invokr-sdk",
      "packageVersion": "0.1.0"
    },
    "haskell-client-codegen": {
      "packageName": "SuperpositionSDK",
      "edition": "2010",
      "version": "0.0.1",
      "service": "com.invokr#InvokrService"
    },
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
    },
    "openapi": {
      "service": "com.invokr#InvokrService",
      "protocol": "aws.protocols#restJson1"
    }
  }
}
```

| Plugin | Output | Package Name |
|--------|--------|-------------|
| `typescript-client-codegen` | TypeScript SDK | `invokr-sdk` |
| `rust-client-codegen` | Rust SDK | `invokr_sdk` |
| `haskell-client-codegen` | Haskell SDK | `SuperpositionSDK` |
| `openapi` | OpenAPI spec | — |

## Keeping the SDKs in sync

The Smithy models are the authoritative definition of the API contract. When the API changes:

1. Update the Smithy model in `smithy/model/`
2. Regenerate all SDKs
3. Review the diffs
4. Commit the model changes and generated SDKs together

```bash
# Validate model, regenerate, sync to crates/client/
just smithy-build

# Review the resulting diff
git diff -- smithy/ crates/client/
```

:::note
There is no CI guard for drift right now. smithy-rs codegen emits some `pub use` blocks in JVM HashMap iteration order (non-stable across processes), so a naive `git diff --exit-code` check is too noisy to enforce. Always regenerate before committing model changes.
:::

## Available SDKs

### TypeScript SDK

- **Package**: `invokr-sdk` (npm)
- **Generated to**: `smithy/build/smithy/source/typescript-client-codegen/`
- **Used by**: CLI test scripts, dashboard integration
- **Build**: `just build-sdk`

See [TypeScript SDK](./typescript) for details.

### Rust SDK

- **Module**: `invokr_sdk`
- **Location**: `crates/client/` (committed to repo)
- **MSRV**: 1.82 (excluded from workspace build)
- **Runtime**: AWS smithy runtime (orchestrator)
- **Build**: `just smithy-build`

See [Rust SDK](./rust) for details.

### Haskell SDK

- **Package**: `SuperpositionSDK`
- **Example**: `haskell-example/`
- **Build**: `just test-haskell`

See [Haskell SDK](./haskell) for details.

## Just recipes

| Recipe | Description |
|--------|-------------|
| `just smithy-build` | Validate Smithy model, regenerate Rust SDK into `crates/client/` |
| `just build-sdk` | Generate and compile the TypeScript SDK (`invokr-sdk` npm package) |
| `just sdk-refresh` | Regenerate + rebuild + reinstall CLI |
| `just cli-install` | Install CLI dependencies (links to built SDK) |
| `just test-haskell` | Run the Haskell SDK example |

## Regenerating after an API change

When you change the API — add an operation, modify a shape, and so on:

```bash
# 1. Update Smithy models
vim smithy/model/invokr.smithy

# 2. Regenerate Rust SDK
just smithy-build

# 3. Review the generated changes
git diff -- crates/client/

# 4. Build the TypeScript SDK
just build-sdk

# 5. Reinstall CLI (if it uses the SDK)
just sdk-refresh

# 6. Commit everything together
git add smithy/ crates/client/ cli/
git commit -m "feat: update Smithy model and regenerate SDKs"
```

:::tip
Always commit Smithy model changes and generated SDK output in the same PR. Downstream consumers depend on the committed `crates/client/` crate and need the model + generated code to be in sync.
:::
