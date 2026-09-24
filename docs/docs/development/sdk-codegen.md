---
id: sdk-codegen
title: SDK Code Generation
---

# SDK Code Generation

Invokr generates client SDKs from [Smithy](https://smithy.io/) IDL models, which keeps the TypeScript, Rust, and Haskell SDKs and the OpenAPI spec in sync with the API definition. This page covers the code generation workflow.

## Smithy models

The Smithy IDL models live in `smithy/model/` and define the Invokr API surface:

| Model file | Description |
|------------|-------------|
| `main.smithy` | Top-level service definition and operations |
| `common.smithy` | Shared types (metadata, pagination, errors) |
| `jobs.smithy` | Job-related operations and shapes (create, list, get, update, cancel, status, versions, executions) |
| `endpoints.smithy` | Endpoint-related operations and shapes |
| `payload-specs.smithy` | Payload spec operations and shapes |

### smithy-build.json

The `smithy/smithy-build.json` file configures code generation for multiple targets:

- **TypeScript client** — generates a TypeScript SDK
- **Rust client** — generates a Rust SDK (copied to `crates/client/`)
- **Haskell client** — generates a Haskell SDK
- **OpenAPI spec** — generates an OpenAPI 3.x specification

## Validation

Before regenerating SDKs, validate the Smithy models to surface errors with clean messages:

```bash
just smithy-validate
```

This runs:

```bash
cd smithy && smithy validate
```

:::tip
Always run `just smithy-validate` before `just smithy-build`. Model errors caught during validation are much easier to debug than cryptic codegen failures.
:::

## Regeneration

```bash
just smithy-build
```

This runs the full regeneration pipeline:

1. **Validates** Smithy models (`just smithy-validate`)
2. **Builds** the models (`cd smithy && smithy build`)
3. **Wipes** the existing Rust SDK at `crates/client/`
4. **Copies** the freshly generated Rust SDK from `smithy/build/smithy/source/rust-client-codegen/` to `crates/client/`
5. **Restores** the tracked `crates/client/README.md` (which contains a "DO NOT EDIT" warning) that the wipe removed

:::warning
The regeneration process **deletes and replaces** the entire `crates/client/` directory. Any local changes to generated code will be lost. Never edit generated code directly — always modify the Smithy models and regenerate.
:::

### Reviewing generated changes

After regeneration, review the diff:

```bash
git diff -- crates/client
```

The generated Rust SDK is committed to the repository so that downstream consumers can use it without running the Smithy toolchain themselves.

## TypeScript SDK

### Building the TypeScript SDK

```bash
just build-sdk
```

This runs the full TypeScript SDK pipeline:

1. Runs `just smithy-build` (validates, generates, copies Rust SDK)
2. Installs npm dependencies in the generated TypeScript project:
   ```bash
   cd smithy/build/smithy/source/typescript-client-codegen && npm install && npm run build
   ```

### Installing CLI dependencies

```bash
just cli-install
```

This runs `just build-sdk` and then installs the CLI's npm dependencies (in `cli/`), which link to the built TypeScript SDK:

```bash
cd cli && npm install
```

## Full SDK refresh

After modifying Smithy models, run a full refresh to regenerate all SDKs and reinstall the CLI:

```bash
just sdk-refresh
```

This is equivalent to `just build-sdk && just cli-install`.

## Adding new API operations

To add a new operation to the Invokr API:

### 1. Edit the Smithy model

Add the operation and its input/output shapes to the appropriate `.smithy` file in `smithy/model/`:

```smithy
@http(method: "POST", uri: "/v1/my-resource", code: 201)
operation CreateMyResource {
    input: CreateMyResourceInput,
    output: CreateMyResourceOutput,
    errors: [BadRequestError, ConflictError]
}

structure CreateMyResourceInput {
    @required
    name: String,
    @required
    data: Document,
}

structure CreateMyResourceOutput {
    @required
    id: String,
    name: String,
    created_at: Timestamp,
}
```

### 2. Validate the model

```bash
just smithy-validate
```

Fix any validation errors before proceeding.

### 3. Regenerate SDKs

```bash
just smithy-build
```

This generates the updated Rust, TypeScript, and Haskell SDKs, plus the OpenAPI spec.

### 4. Build the TypeScript SDK

```bash
just build-sdk
```

### 5. Implement the API handler

Add the Rust handler in `crates/api/src/handlers/` and register the route in `crates/api/src/router.rs`. The generated Rust SDK types in `crates/client/` can be used as reference for the request/response shapes.

### 6. Commit all changes

Commit the Smithy model changes, generated SDK code, and API handler implementation in the same PR:

```bash
git add smithy/model/ crates/client/ crates/api/
git commit -m "Add CreateMyResource operation"
```

:::important
Always commit the Smithy model changes and the generated SDK code (`crates/client/`) in the same PR, so reviewers can see both the model change and its effect on the generated code.
:::

## Generated artifacts

| Artifact | Location | Committed? |
|----------|----------|------------|
| Rust SDK | `crates/client/` | Yes |
| TypeScript SDK | `smithy/build/smithy/source/typescript-client-codegen/` | No (regenerated) |
| Haskell SDK | `smithy/build/smithy/source/haskell-client-codegen/` | No (regenerated) |
| OpenAPI spec | `smithy/build/smithy/source/openapi/` | No (regenerated) |
| CLI | `cli/` | Yes (uses TypeScript SDK at build time) |

:::note
The generated Rust SDK at `crates/client/` is committed to the repository so that:
1. Downstream Rust consumers can depend on it without running the Smithy toolchain
2. CI builds don't require the Smithy CLI
3. Code reviews can see exactly what changed in the generated code

The TypeScript and Haskell SDKs are not committed because they are rebuilt from source by `just build-sdk` and the Haskell example respectively.
:::

## Maven repositories

The Smithy build uses two Maven repositories for codegen plugins:

```bash
export SMITHY_MAVEN_REPOS="https://repo.maven.apache.org/maven2|https://sandbox.assets.juspay.in/smithy/m2"
```

This is set automatically in the justfile. The second repository hosts Juspay's Smithy codegen plugins.

## See also

- [Development Setup](./setup) — initial setup including SDK installation
- [Building](./building) — build commands and feature flags
- [Testing](./testing) — testing the generated SDKs via E2E tests
