//! Shared job orchestration: create + cancel, used by the REST handler and the
//! library client alike. See the module docs in `service.rs` for why this exists.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::db::{self, scoped::scoped_transaction, DbContext};
use crate::error::AppError;
use crate::metrics as m;
use crate::models::{endpoint::EndpointType, Execution, Job};

/// How a job should be triggered, with the parameters each variant needs.
///
/// `next_run_at` for CRON is supplied by the caller because the two adapters
/// resolve it differently: REST computes it from a cron expression + timezone,
/// while the library client takes an explicit `first_run_at`. The core just
/// persists what it is given.
pub enum CreateTrigger<'a> {
    Immediate,
    Delayed {
        run_at: DateTime<Utc>,
    },
    Cron {
        expression: &'a str,
        timezone: &'a str,
        starts_at: Option<DateTime<Utc>>,
        ends_at: Option<DateTime<Utc>>,
        next_run_at: DateTime<Utc>,
    },
}

/// Everything the core needs to create a job. `endpoint_type`, `max_attempts`
/// resolution, validation and idempotency are handled inside [`create_job`] from
/// the endpoint row, so callers pass only their raw inputs.
pub struct CreateJobParams<'a> {
    pub endpoint: &'a str,
    pub idempotency_key: Option<&'a str>,
    pub input: Option<&'a serde_json::Value>,
    /// Per-job override; used when positive, otherwise the endpoint's retry policy.
    pub max_attempts_override: Option<i64>,
    pub trigger: CreateTrigger<'a>,
}

/// Result of [`create_job`]. `Existing` is the idempotency short-circuit: a job
/// with the same key already exists, so nothing new was created.
// A one-shot return value, matched immediately by the caller and never stored,
// so the size gap between the variants costs nothing.
#[allow(clippy::large_enum_variant)]
pub enum CreateOutcome {
    Created {
        job: Job,
        /// `Some` for IMMEDIATE/DELAYED (an execution is materialized eagerly);
        /// `None` for CRON (executions materialize later via pg_cron ticks).
        execution_id: Option<String>,
        execution_status: Option<String>,
        execution_created_at: Option<DateTime<Utc>>,
    },
    Existing {
        job: Job,
        execution: Option<Execution>,
    },
}

/// Create a job (and, for CRON, register its pg_cron schedule) atomically.
///
/// Owns a single `scoped_transaction` for the whole operation: endpoint fetch →
/// INTERNAL guard → input validation → max-attempts resolution → idempotency
/// short-circuit → insert(s) → (CRON) pg_cron registration → commit. This is the
/// one implementation shared by REST and library mode.
// `drop(db)` below is not about running a destructor — it releases the
// DbContext's `&mut tx` borrow so pg_cron can run on the same transaction.
#[allow(clippy::drop_non_drop)]
pub async fn create_job(
    pool: &PgPool,
    prefix: &str,
    schema_name: &str,
    params: &CreateJobParams<'_>,
) -> Result<CreateOutcome, AppError> {
    let mut tx = scoped_transaction(pool, schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut tx, prefix);

    // Endpoint must exist.
    let ep = db::endpoints::get(&mut db, params.endpoint)
        .await?
        .ok_or_else(|| AppError::EndpointNotFound(params.endpoint.to_string()))?;

    // INTERNAL endpoints back invokr-driven jobs (today: the dogfooded reaper)
    // and are not user-creatable — the same invariant the REST layer enforces.
    if EndpointType::from_str_val(&ep.endpoint_type) == Some(EndpointType::INTERNAL) {
        return Err(AppError::InvalidRequest(format!(
            "Endpoint '{}' is internal and cannot be used for user-created jobs",
            params.endpoint
        )));
    }

    // Per-job override wins when provided and positive; otherwise the endpoint's policy.
    let retry_policy = ep.get_retry_policy();
    let max_attempts = params
        .max_attempts_override
        .filter(|&n| n > 0)
        .unwrap_or(retry_policy.max_attempts);

    // Validate input against the endpoint's payload spec, when both are present.
    if let Some(ref ps_name) = ep.payload_spec_ref {
        if let Some(input) = params.input {
            let spec = db::payload_specs::get(&mut db, ps_name)
                .await?
                .ok_or_else(|| AppError::InvalidPayloadSpecRef(ps_name.clone()))?;
            validate_input(input, &spec.schema_json)?;
        }
    }

    // Idempotency short-circuit: on key reuse, return the existing job instead of
    // colliding on the unique index.
    if let Some(key) = params.idempotency_key {
        if let Some(existing) = db::jobs::get_by_idempotency(&mut db, params.endpoint, key).await? {
            let execution = db::executions::get_for_job(&mut db, &existing.job_id).await?;
            return Ok(CreateOutcome::Existing {
                job: existing,
                execution,
            });
        }
    }

    let (outcome, trigger_label) = match &params.trigger {
        CreateTrigger::Immediate => {
            let r = db::jobs::create_immediate(
                &mut db,
                params.endpoint,
                &ep.endpoint_type,
                params.idempotency_key,
                params.input,
                max_attempts,
            )
            .await
            .map_err(|e| map_insert_error(e, params.endpoint))?;
            (created_from(r), "IMMEDIATE")
        }
        CreateTrigger::Delayed { run_at } => {
            let r = db::jobs::create_delayed(
                &mut db,
                params.endpoint,
                &ep.endpoint_type,
                params.idempotency_key,
                params.input,
                *run_at,
                max_attempts,
            )
            .await
            .map_err(|e| map_insert_error(e, params.endpoint))?;
            (created_from(r), "DELAYED")
        }
        CreateTrigger::Cron {
            expression,
            timezone,
            starts_at,
            ends_at,
            next_run_at,
        } => {
            let job = db::jobs::create_cron(
                &mut db,
                params.endpoint,
                &ep.endpoint_type,
                params.idempotency_key,
                params.input,
                expression,
                timezone,
                *starts_at,
                *ends_at,
                *next_run_at,
            )
            .await
            .map_err(|e| map_insert_error(e, params.endpoint))?;
            // Release the &mut borrow of tx held via db, then register pg_cron on
            // the SAME transaction so the job row and its schedule commit together.
            drop(db);
            db::jobs::register_pg_cron_conn(&mut tx, prefix, schema_name, &job.job_id, expression)
                .await
                .map_err(AppError::from)?;
            (
                CreateOutcome::Created {
                    job,
                    execution_id: None,
                    execution_status: None,
                    execution_created_at: None,
                },
                "CRON",
            )
        }
    };

    tx.commit().await.map_err(AppError::from)?;

    metrics::counter!(m::JOBS_CREATED_TOTAL,
        "trigger_type" => trigger_label,
        "endpoint" => params.endpoint.to_string(),
        "schema" => schema_name.to_string(),
    )
    .increment(1);

    Ok(outcome)
}

/// Cancel a job and, for CRON jobs, unschedule its pg_cron entry — atomically on
/// one transaction. For non-CRON jobs, also cancels PENDING/QUEUED executions.
/// Returns the retired job. Shared by REST and library mode.
// `drop(db)` below is not about running a destructor — it releases the
// DbContext's `&mut tx` borrow so pg_cron can run on the same transaction.
#[allow(clippy::drop_non_drop)]
pub async fn cancel_job(
    pool: &PgPool,
    prefix: &str,
    schema_name: &str,
    job_id: &str,
) -> Result<Job, AppError> {
    let mut tx = scoped_transaction(pool, schema_name)
        .await
        .map_err(AppError::from)?;
    let mut db = DbContext::new(&mut tx, prefix);

    let job = db::jobs::get(&mut db, job_id)
        .await?
        .ok_or_else(|| AppError::JobNotFound(job_id.to_string()))?;

    if job.status == "RETIRED" {
        return Err(AppError::Conflict("Job is already retired".into()));
    }
    // INTERNAL jobs are invokr-managed (today: the dogfooded reaper). Cancelling
    // one would stop the reaper from sweeping this workspace, with no surviving
    // bootstrap path to bring it back.
    if EndpointType::from_str_val(&job.endpoint_type) == Some(EndpointType::INTERNAL) {
        return Err(AppError::Conflict(
            "Internal invokr jobs cannot be cancelled through the API".into(),
        ));
    }

    if job.trigger_type != "CRON" {
        db::executions::cancel_pending_for_job(&mut db, job_id).await?;
    }

    let cancelled = db::jobs::cancel(&mut db, job_id)
        .await?
        .ok_or_else(|| AppError::Conflict("Job could not be cancelled".into()))?;

    drop(db);

    // Unschedule from pg_cron on the same tx as the status flip, so the cancel and
    // the unschedule commit (or roll back) together — no RETIRED row left with a
    // live pg_cron entry.
    if job.trigger_type == "CRON" {
        db::jobs::unregister_pg_cron_conn(&mut tx, schema_name, job_id)
            .await
            .map_err(AppError::from)?;
    }

    tx.commit().await.map_err(AppError::from)?;

    Ok(cancelled)
}

fn created_from(r: db::jobs::CreateJobResult) -> CreateOutcome {
    CreateOutcome::Created {
        job: r.job,
        execution_id: Some(r.execution_id),
        execution_status: Some(r.execution_status),
        execution_created_at: Some(r.execution_created_at),
    }
}

/// Map a failed job insert, shared by all three triggers. The idempotency
/// short-circuit above runs before the insert, so these are races it cannot see:
///
/// - `UniqueViolation` -> 409: another request won with the same key.
/// - `ForeignKeyViolation` -> 404: `jobs` has exactly one foreign key, so the
///   endpoint was deleted after the lookup — the answer the lookup itself would
///   have given.
///
/// Classified by `kind()`, not `constraint().is_some()`: `jobs` also carries CHECK
/// constraints, and Postgres names those too, so the looser test would report any
/// of them as a duplicate key. Everything else is an honest 500.
fn map_insert_error(e: sqlx::Error, endpoint: &str) -> AppError {
    use sqlx::error::ErrorKind;
    match e {
        sqlx::Error::Database(ref db_err) if db_err.kind() == ErrorKind::UniqueViolation => {
            AppError::Conflict("Job with this idempotency key already exists".into())
        }
        sqlx::Error::Database(ref db_err) if db_err.kind() == ErrorKind::ForeignKeyViolation => {
            AppError::EndpointNotFound(endpoint.to_string())
        }
        _ => AppError::from(e),
    }
}

/// Validate `input` against a JSON Schema, producing `AppError` on any failure.
/// Moved here (from the REST handler) so both modes validate identically.
fn validate_input(input: &serde_json::Value, schema: &serde_json::Value) -> Result<(), AppError> {
    let compiled = jsonschema::JSONSchema::compile(schema)
        .map_err(|e| AppError::InvalidSchema(format!("{}", e)))?;

    if let Err(errors) = compiled.validate(input) {
        let msgs: Vec<String> = errors.map(|e| e.to_string()).collect();
        return Err(AppError::InputValidationFailed(msgs.join("; ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::error::{DatabaseError, ErrorKind};
    use std::borrow::Cow;
    use std::fmt;

    /// A stand-in for a Postgres error. Every kind reports a constraint name, as
    /// Postgres does for unique, foreign-key and CHECK violations alike — which is
    /// exactly why classifying on `constraint().is_some()` was wrong.
    #[derive(Debug)]
    enum Fake {
        Unique,
        ForeignKey,
        Check,
    }

    impl fmt::Display for Fake {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{self:?}")
        }
    }

    impl std::error::Error for Fake {}

    impl DatabaseError for Fake {
        fn message(&self) -> &str {
            "fake"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            None
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn constraint(&self) -> Option<&str> {
            Some("some_constraint")
        }
        fn kind(&self) -> ErrorKind {
            match self {
                Fake::Unique => ErrorKind::UniqueViolation,
                Fake::ForeignKey => ErrorKind::ForeignKeyViolation,
                Fake::Check => ErrorKind::CheckViolation,
            }
        }
    }

    fn db_err(kind: Fake) -> sqlx::Error {
        sqlx::Error::Database(Box::new(kind))
    }

    #[test]
    fn unique_violation_is_an_idempotency_conflict() {
        let err = map_insert_error(db_err(Fake::Unique), "ep");
        assert!(matches!(err, AppError::Conflict(_)), "{err:?}");
    }

    #[test]
    fn foreign_key_violation_means_the_endpoint_vanished() {
        let err = map_insert_error(db_err(Fake::ForeignKey), "ep");
        assert!(
            matches!(err, AppError::EndpointNotFound(ref e) if e == "ep"),
            "{err:?}"
        );
    }

    #[test]
    fn check_violation_is_not_reported_as_a_conflict() {
        let err = map_insert_error(db_err(Fake::Check), "ep");
        assert!(
            !matches!(err, AppError::Conflict(_) | AppError::EndpointNotFound(_)),
            "{err:?}"
        );
    }

    #[test]
    fn non_database_errors_pass_through() {
        let err = map_insert_error(sqlx::Error::RowNotFound, "ep");
        assert!(!matches!(err, AppError::Conflict(_)), "{err:?}");
    }
}
