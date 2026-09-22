---
id: haskell
title: Haskell SDK
---

# Haskell SDK

The Haskell SDK is generated from Smithy IDL models via the `in.juspay.smithy.haskell` codegen plugin. It provides a typed client for the Invokr REST API using Haskell's `Network.HTTP.Client` for transport.

## Package

- **Package name**: `InvokrSDK`
- **Smithy plugin**: `haskell-client-codegen`
- **Example project**: `haskell-example/`

### smithy-build.json Configuration

```json
{
  "haskell-client-codegen": {
    "packageName": "InvokrSDK",
    "edition": "2010",
    "version": "0.0.1",
    "service": "com.invokr#InvokrService"
  }
}
```

## Building and Running

The Haskell SDK example is built and run via the justfile:

```bash
# Run the Haskell SDK example (builds + runs the end-to-end test)
just test-haskell
```

### Nix-Shell Based Build

The build uses a Nix-shell environment with GHC 9.6 and the required Haskell packages:

```bash
nix-shell -p ghc haskellPackages.aeson haskellPackages.text \
  haskellPackages.http-client haskellPackages.network-uri
```

### Manual Build

```bash
cd haskell-example/
cabal build
cabal run haskell-example
```

## Example Client

The example at `haskell-example/app/Main.hs` mirrors the TypeScript CLI test (`test-immediate.ts`) and demonstrates the full lifecycle of a Invokr job:

1. Build a `InvokrServiceClient`
2. Create an HTTP endpoint pointing at the mock server
3. Schedule an `IMMEDIATE` job targeting that endpoint
4. Poll `ListJobExecutions` until the job reaches a terminal state
5. Print the final execution status
6. Clean up: cancel the job and delete the endpoint

### Client Setup

```haskell
import qualified Com.Invokr.InvokrServiceClient as Client
import qualified Network.HTTP.Client as HTTP
import qualified Network.URI as URI

main :: IO ()
main = do
    manager <- HTTP.newManager HTTP.defaultManagerSettings

    let clientResult = Client.build $ do
            Client.setEndpointuri (parseUri "http://localhost:8080")
            Client.setHttpmanager manager
            Client.setBearerauth (Just (Client.BearerAuth "dev-api-key"))

    client <- case clientResult of
        Left err -> error $ "Failed to build client: " <> show err
        Right c  -> return c
```

### Creating an Endpoint

```haskell
import qualified Com.Invokr.Command.CreateEndpoint as CreateEndpoint
import qualified Com.Invokr.Model.CreateEndpointInput as CreateEndpointInput
import qualified Com.Invokr.Model.EndpointTypeEnum as EndpointTypeEnum

let endpointSpec :: Value
    endpointSpec = object ["url" .= ("http://localhost:9999/success" :: String)]

endpointResult <- CreateEndpoint.createEndpoint client $ do
    CreateEndpointInput.setName "haskell-example-endpoint"
    CreateEndpointInput.setEndpointType EndpointTypeEnum.HTTP
    CreateEndpointInput.setSpec endpointSpec
```

### Creating a Job

```haskell
import qualified Com.Invokr.Command.CreateJob as CreateJob
import qualified Com.Invokr.Model.CreateJobInput as CreateJobInput
import qualified Com.Invokr.Model.TriggerTypeEnum as TriggerTypeEnum

let jobPayload :: Value
    jobPayload = object
        [ "task"    .= ("send-report" :: String)
        , "user_id" .= (42 :: Int)
        , "report"  .= ("monthly-summary" :: String)
        ]

jobResult <- CreateJob.createJob client $ do
    CreateJobInput.setEndpoint "haskell-example-endpoint"
    CreateJobInput.setTrigger   TriggerTypeEnum.IMMEDIATE
    CreateJobInput.setInput     (Just jobPayload)
```

### Polling for Execution

```haskell
import qualified Com.Invokr.Command.ListJobExecutions as ListJobExecutions
import qualified Com.Invokr.Model.ExecutionStatusEnum as ExecutionStatusEnum

isTerminal :: ExecutionStatusEnum.ExecutionStatusEnum -> Bool
isTerminal ExecutionStatusEnum.SUCCESS   = True
isTerminal ExecutionStatusEnum.FAILED    = True
isTerminal ExecutionStatusEnum.CANCELLED = True
isTerminal _                             = False

pollExecution :: Client.InvokrServiceClient -> T.Text -> Int -> IO (Maybe ExecutionResource)
pollExecution client jobId remaining
    | remaining <= 0 = return Nothing
    | otherwise = do
        result <- ListJobExecutions.listJobExecutions client $ do
            ListJobExecutionsInput.setJobId jobId
        case result of
            Left err -> return Nothing
            Right out -> do
                let executions = ListJobExecutionsOutput.data' out
                case filter (isTerminal . ExecutionResource.status) executions of
                    (exec : _) -> return (Just exec)
                    [] -> do
                        threadDelay 500000  -- 500ms
                        pollExecution client jobId (remaining - 500000)
```

### Cleanup

```haskell
import qualified Com.Invokr.Command.CancelJob as CancelJob
import qualified Com.Invokr.Command.DeleteEndpoint as DeleteEndpoint

cleanup :: Client.InvokrServiceClient -> Maybe T.Text -> T.Text -> IO ()
cleanup client mJobId epName = do
    case mJobId of
        Just jobId -> do
            cancelResult <- CancelJob.cancelJob client $ do
                CancelJobInput.setJobId jobId
            -- ...
        Nothing -> return ()

    deleteResult <- DeleteEndpoint.deleteEndpoint client $ do
        DeleteEndpointInput.setName epName
    -- ...
```

## SDK Module Structure

The generated Haskell SDK organizes modules by concern:

| Module Pattern | Description |
|----------------|-------------|
| `Com.Invokr.InvokrServiceClient` | Main client type and builder |
| `Com.Invokr.Command.*` | Operation functions (e.g., `CreateJob.createJob`) |
| `Com.Invokr.Model.*Input` | Input builders for each operation |
| `Com.Invokr.Model.*Output` | Output accessors for each operation |
| `Com.Invokr.Model.*Resource` | Resource types (e.g., `JobResource`, `ExecutionResource`) |
| `Com.Invokr.Model.*Enum` | Enum types (e.g., `EndpointTypeEnum`, `TriggerTypeEnum`, `ExecutionStatusEnum`) |

## Configuration

The example uses hardcoded defaults that match the local development environment:

| Setting | Value | Description |
|---------|-------|-------------|
| `invokrUrl` | `http://localhost:8080` | Invokr API base URL |
| `mockUrl` | `http://localhost:9999` | Mock server URL (for endpoint target) |
| `apiKey` | `dev-api-key` | Bearer token for authentication |
| `endpointName` | `haskell-example-endpoint` | Name for the test endpoint |
| `pollIntervalUs` | `500000` (500ms) | Poll interval for execution status |
| `pollTimeoutUs` | `30000000` (30s) | Maximum wait time for execution |

## Prerequisites

The Haskell example requires all Invokr services to be running:

```bash
# Start all services
just dev

# Or individually:
docker compose up -d postgres    # PostgreSQL
cargo run -p invokr-api           # API server (port 8080)
cargo run -p invokr-worker        # Worker (metrics on :9090)
cargo run -p invokr-mock-server   # Mock server (port 9999)
```

:::tip
The Haskell SDK uses the `Network.HTTP.Client` package for HTTP transport and `Data.Aeson` for JSON serialization. The builder pattern (using `set*` functions in a `do` block) mirrors the approach used by the AWS SDK for Haskell.
:::

## Related Pages

- [SDK Overview](./overview) — All SDKs and the Smithy codegen pipeline
- [TypeScript SDK](./typescript) — TypeScript SDK for JS/TS consumers
- [Rust SDK](./rust) — Rust SDK for Rust consumers
