use super::*;

// `create_job` returns the execution id for IMMEDIATE/DELAYED and the job id for
// CRON. An idempotent replay must return the same id as the first call.

#[test]
fn immediate_returns_its_execution_id() {
    let id = created_job_ref("IMMEDIATE", "job-1".into(), Some("exec-1".into()));
    assert_eq!(id, "exec-1");
}

#[test]
fn cron_returns_the_job_id_even_once_executions_exist() {
    // The replay path looks up the job's latest execution; after the first tick a
    // CRON job has one, and returning it would hand back a different id than the
    // first call did.
    let id = created_job_ref("CRON", "job-1".into(), Some("exec-from-a-tick".into()));
    assert_eq!(id, "job-1");
}

#[test]
fn cron_before_any_tick_returns_the_job_id() {
    let id = created_job_ref("CRON", "job-1".into(), None);
    assert_eq!(id, "job-1");
}

/// Library-mode round trips against a real database. Ignored by default: they
/// need Postgres with `pg_cron` and the base migrations applied, e.g.
/// `INVOKR_TEST_DATABASE_URL=... cargo test -p invokr-worker -- --ignored`.
/// Each test provisions its own schema and removes it, with its pg_cron
/// entries, afterwards.
mod db {
    use super::*;
    use chrono::Duration;

    const PREFIX: &str = "lib_";
    const KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    const REAPER: &str = "invokr.reaper";

    async fn setup() -> (InvokrLibraryClient, String) {
        let url = std::env::var("INVOKR_TEST_DATABASE_URL").expect(
            "set INVOKR_TEST_DATABASE_URL to a Postgres with pg_cron and the base migrations",
        );
        let client = InvokrLibraryClient::from_database_url(&url, 4, PREFIX, KEY, None)
            .await
            .expect("connect");
        let schema = format!(
            "parity_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        );
        (client, schema)
    }

    async fn teardown(client: &InvokrLibraryClient, schema: &str) {
        sqlx::query("SELECT cron.unschedule(jobid) FROM cron.job WHERE jobname LIKE $1")
            .bind(format!("invokr_{schema}_%"))
            .execute(client.pool())
            .await
            .expect("unschedule");
        sqlx::query(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"))
            .execute(client.pool())
            .await
            .expect("drop schema");
    }

    async fn scalar(client: &InvokrLibraryClient, sql: &str) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(sql).fetch_one(client.pool()).await?)
    }

    async fn active_reapers(client: &InvokrLibraryClient, schema: &str) -> anyhow::Result<i64> {
        scalar(
            client,
            &format!(
                "SELECT count(*) FROM \"{schema}\".{PREFIX}jobs \
                 WHERE endpoint = '{REAPER}' AND status = 'ACTIVE'"
            ),
        )
        .await
    }

    async fn cron_entries(client: &InvokrLibraryClient, schema: &str) -> anyhow::Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT count(*) FROM cron.job WHERE jobname LIKE $1")
                .bind(format!("invokr_{schema}_%"))
                .fetch_one(client.pool())
                .await?,
        )
    }

    #[tokio::test]
    #[ignore = "requires Postgres with pg_cron: set INVOKR_TEST_DATABASE_URL"]
    async fn provision_workspace_is_idempotent() {
        let (client, schema) = setup().await;
        let result: anyhow::Result<()> = async {
            client.provision_workspace(&schema).await?;
            client.provision_workspace(&schema).await?;
            anyhow::ensure!(active_reapers(&client, &schema).await? == 1, "reaper jobs");
            anyhow::ensure!(
                cron_entries(&client, &schema).await? == 1,
                "pg_cron entries"
            );
            Ok(())
        }
        .await;
        teardown(&client, &schema).await;
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres with pg_cron: set INVOKR_TEST_DATABASE_URL"]
    async fn provisioning_backfills_the_reaper_into_an_existing_workspace() {
        let (client, schema) = setup().await;
        let result: anyhow::Result<()> = async {
            // A workspace provisioned before library mode installed a reaper.
            invokr_common::db::workspaces::provision_schema(client.pool(), &schema, PREFIX).await?;
            anyhow::ensure!(
                active_reapers(&client, &schema).await? == 0,
                "no reaper yet"
            );
            client.provision_workspace(&schema).await?;
            anyhow::ensure!(
                active_reapers(&client, &schema).await? == 1,
                "reaper installed"
            );
            Ok(())
        }
        .await;
        teardown(&client, &schema).await;
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres with pg_cron: set INVOKR_TEST_DATABASE_URL"]
    async fn the_reaper_cannot_be_overwritten_or_cancelled() {
        let (client, schema) = setup().await;
        let result: anyhow::Result<()> = async {
            client.provision_workspace(&schema).await?;
            let spec = serde_json::json!({ "url": "http://localhost:9999/hook" });
            let overwrite = client
                .register_endpoint(&schema, REAPER, "HTTP", spec.clone(), None)
                .await;
            anyhow::ensure!(
                overwrite.is_err(),
                "upsert over the reaper endpoint succeeded"
            );
            let internal = client
                .register_endpoint(&schema, "mine", "INTERNAL", spec, None)
                .await;
            anyhow::ensure!(internal.is_err(), "registered an INTERNAL endpoint");

            let job_id: String = sqlx::query_scalar(&format!(
                "SELECT job_id FROM \"{schema}\".{PREFIX}jobs WHERE endpoint = '{REAPER}'"
            ))
            .fetch_one(client.pool())
            .await?;
            anyhow::ensure!(
                client.cancel_job(&schema, &job_id).await.is_err(),
                "cancelled the reaper"
            );
            anyhow::ensure!(
                active_reapers(&client, &schema).await? == 1,
                "reaper still active"
            );
            anyhow::ensure!(
                cron_entries(&client, &schema).await? == 1,
                "reaper still scheduled"
            );
            Ok(())
        }
        .await;
        teardown(&client, &schema).await;
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres with pg_cron: set INVOKR_TEST_DATABASE_URL"]
    async fn a_cron_replay_returns_the_first_calls_id() {
        let (client, schema) = setup().await;
        let result: anyhow::Result<()> = async {
            client.provision_workspace(&schema).await?;
            let spec = serde_json::json!({ "url": "http://localhost:9999/hook" });
            client
                .register_endpoint(&schema, "hook", "HTTP", spec, None)
                .await?;
            let cron = || JobTrigger::Cron {
                expression: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                starts_at: None,
                ends_at: None,
                first_run_at: Utc::now() + Duration::minutes(5),
            };
            let input = serde_json::json!({});
            let first = client
                .create_job(&schema, "hook", input.clone(), 3, cron(), Some("k1"))
                .await?;

            // What a pg_cron tick leaves behind: an execution for the job.
            sqlx::query(&format!(
                "INSERT INTO \"{schema}\".{PREFIX}executions (job_id, endpoint, endpoint_type) \
                 VALUES ($1, 'hook', 'HTTP')"
            ))
            .bind(&first)
            .execute(client.pool())
            .await?;

            let replay = client
                .create_job(&schema, "hook", input, 3, cron(), Some("k1"))
                .await?;
            anyhow::ensure!(
                replay == first,
                "replay returned {replay}, first call {first}"
            );
            // The reaper's entry plus this job's; the replay registered nothing.
            anyhow::ensure!(
                cron_entries(&client, &schema).await? == 2,
                "pg_cron entries"
            );
            Ok(())
        }
        .await;
        teardown(&client, &schema).await;
        result.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres with pg_cron: set INVOKR_TEST_DATABASE_URL"]
    async fn endpoint_refs_are_checked_and_land_in_the_right_columns() {
        let (client, schema) = setup().await;
        let result: anyhow::Result<()> = async {
            client.provision_workspace(&schema).await?;
            let spec = serde_json::json!({ "url": "http://localhost:9999/hook" });
            let missing = client
                .register_endpoint_with_refs(&schema, "hook", "HTTP", spec.clone(), None, Some("nope"), None)
                .await;
            anyhow::ensure!(missing.is_err(), "registered a dangling payload_spec ref");

            for sql in [
                format!("INSERT INTO \"{schema}\".{PREFIX}payload_specs (name, schema_json) VALUES ('ps', '{{}}')"),
                format!("INSERT INTO \"{schema}\".{PREFIX}configs (name, values_json) VALUES ('cfg', '{{}}'), ('cfg2', '{{}}')"),
            ] {
                sqlx::query(&sql).execute(client.pool()).await?;
            }

            let refs = || async {
                sqlx::query_as::<_, (Option<String>, Option<String>)>(&format!(
                    "SELECT payload_spec_ref, config_ref FROM \"{schema}\".{PREFIX}endpoints \
                     WHERE name = 'hook'"
                ))
                .fetch_one(client.pool())
                .await
            };

            // Create path: create() takes (payload_spec_ref, config_ref).
            client
                .register_endpoint_with_refs(&schema, "hook", "HTTP", spec.clone(), None, Some("ps"), Some("cfg"))
                .await?;
            anyhow::ensure!(refs().await? == (Some("ps".into()), Some("cfg".into())), "create");

            // Update path: update() takes them in the opposite order; None keeps a ref.
            client
                .register_endpoint_with_refs(&schema, "hook", "HTTP", spec, None, None, Some("cfg2"))
                .await?;
            anyhow::ensure!(refs().await? == (Some("ps".into()), Some("cfg2".into())), "update");
            Ok(())
        }
        .await;
        teardown(&client, &schema).await;
        result.unwrap();
    }
}
