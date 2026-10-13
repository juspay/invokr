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
