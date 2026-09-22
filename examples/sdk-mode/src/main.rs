//! SDK-mode example: talk to a running Invokr over HTTP with the generated
//! Rust client.
//!
//! This is the Rust counterpart of `cli/src/test-immediate.ts` and
//! `haskell-example/app/Main.hs` — the same six steps against the same API, so
//! the three SDKs can be read side by side.
//!
//! It is the *client* story. For the other one — Invokr embedded in your own
//! process with no API to call — see `examples/library-mode`.
//!
//! Prerequisites:
//!   1. `just setup`                   — database + migrations
//!   2. `just dev`                     — API, worker and mock server
//!   3. `./scripts/setup-dev-tenant.sh` — writes the org/workspace ids to .env
//!
//! Then:
//!   just example-sdk-mode

use anyhow::{anyhow, Context, Result};
use aws_smithy_types::Document;
use invokr_sdk::config::{Config, Token};
use invokr_sdk::types::{EndpointTypeEnum, ExecutionStatusEnum, TriggerTypeEnum};
use invokr_sdk::Client;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// `Document` is smithy's untyped JSON value. There is no `json!` macro for it,
/// so these two keep the request bodies below readable.
fn obj(pairs: impl IntoIterator<Item = (&'static str, Document)>) -> Document {
    Document::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<HashMap<_, _>>(),
    )
}

fn s(value: impl Into<String>) -> Document {
    Document::String(value.into())
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn required(key: &str) -> Result<String> {
    std::env::var(key)
        .with_context(|| format!("{key} is not set — run ./scripts/setup-dev-tenant.sh"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let invokr_url = env_or("INVOKR_URL", "http://localhost:8080");
    let mock_url = env_or("MOCK_URL", "http://localhost:9999");
    let api_key = env_or("INVOKR_API_KEY", "dev-api-key");
    let org_id = required("INVOKR_ORG_ID")?;
    let workspace_id = required("INVOKR_WORKSPACE_ID")?;

    // 1. Build the client. Auth is a bearer token; tenancy travels as two
    //    headers, which the generated operations take as ordinary inputs.
    //
    //    `behavior_version_latest()` is not optional: smithy-rs panics at
    //    client construction without a behavior version, and the panic does
    //    not appear until you actually run the thing.
    let client = Client::from_conf(
        Config::builder()
            .behavior_version_latest()
            .endpoint_url(&invokr_url)
            .bearer_token(Token::new(api_key, None))
            .build(),
    );

    let endpoint_name = format!("sdk-example-{}", std::process::id());
    let idempotency_key = format!("sdk-example-{}", std::process::id());

    // 2. Describe the call Invokr should make on our behalf.
    println!("creating endpoint {endpoint_name} → {mock_url}/success");
    client
        .create_endpoint()
        .org_id(&org_id)
        .workspace_id(&workspace_id)
        .name(&endpoint_name)
        .endpoint_type(EndpointTypeEnum::Http)
        .spec(obj([
            ("method", s("POST")),
            ("url", s(format!("{mock_url}/success"))),
            ("headers", obj([("Content-Type", s("application/json"))])),
        ]))
        .send()
        .await
        .context("create_endpoint failed")?;

    // 3. Schedule it. IMMEDIATE means the execution row is written QUEUED and
    //    the next worker poll picks it up.
    let job = client
        .create_job()
        .org_id(&org_id)
        .workspace_id(&workspace_id)
        .endpoint(&endpoint_name)
        .trigger(TriggerTypeEnum::Immediate)
        .idempotency_key(&idempotency_key)
        .input(obj([
            ("message", s("hello from the Rust SDK")),
            ("source", s("examples/sdk-mode")),
        ]))
        .send()
        .await
        .context("create_job failed")?
        .data;

    println!("job {} created ({:?})", job.job_id, job.status);

    // 4. Wait for the execution to reach a terminal state.
    let deadline = Instant::now() + POLL_TIMEOUT;
    let execution = loop {
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out after {POLL_TIMEOUT:?} — is the worker running?"
            ));
        }

        let executions = client
            .list_job_executions()
            .org_id(&org_id)
            .workspace_id(&workspace_id)
            .job_id(&job.job_id)
            .send()
            .await
            .context("list_job_executions failed")?
            .data;

        match executions.first() {
            Some(exec)
                if matches!(
                    exec.status,
                    ExecutionStatusEnum::Success
                        | ExecutionStatusEnum::Failed
                        | ExecutionStatusEnum::Cancelled
                ) =>
            {
                // The list route returns a leaner ExecutionResource than the
                // get route: duration_ms, worker_id and endpoint are left
                // unset there. Fetch the full record for the summary below.
                break client
                    .get_execution()
                    .org_id(&org_id)
                    .workspace_id(&workspace_id)
                    .execution_id(&exec.execution_id)
                    .send()
                    .await
                    .context("get_execution failed")?
                    .data;
            }
            Some(exec) => println!(
                "  {} is {:?}, attempt {}/{}",
                &exec.execution_id[..8.min(exec.execution_id.len())],
                exec.status,
                exec.attempt_count,
                exec.max_attempts
            ),
            None => println!("  no execution yet"),
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    };

    println!(
        "\nexecution {} — {:?} in {}ms over {} attempt(s)",
        execution.execution_id,
        execution.status,
        execution
            .duration_ms
            .map(|d| d.to_string())
            .unwrap_or_else(|| "?".into()),
        execution.attempt_count,
    );

    // 5. Every attempt is a row, not a log line.
    let attempts = client
        .list_execution_attempts()
        .org_id(&org_id)
        .workspace_id(&workspace_id)
        .execution_id(&execution.execution_id)
        .send()
        .await
        .context("list_execution_attempts failed")?
        .data;

    for attempt in &attempts {
        println!(
            "  #{} {:?} {}ms",
            attempt.attempt_number,
            attempt.status,
            attempt
                .duration_ms
                .map(|d| d.to_string())
                .unwrap_or_else(|| "?".into()),
        );
    }

    // 6. Put the workspace back the way we found it.
    let _ = client
        .cancel_job()
        .org_id(&org_id)
        .workspace_id(&workspace_id)
        .job_id(&job.job_id)
        .send()
        .await;
    let _ = client
        .delete_endpoint()
        .org_id(&org_id)
        .workspace_id(&workspace_id)
        .name(&endpoint_name)
        .send()
        .await;

    if execution.status == ExecutionStatusEnum::Success {
        println!("\ndone");
        Ok(())
    } else {
        Err(anyhow!("execution ended {:?}", execution.status))
    }
}
