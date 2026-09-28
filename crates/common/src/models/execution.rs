use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExecutionStatus {
    PENDING,
    QUEUED,
    RUNNING,
    RETRYING,
    SUCCESS,
    FAILED,
    CANCELLED,
    WAITING,
    POLLING,
}

impl ExecutionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PENDING => "PENDING",
            Self::QUEUED => "QUEUED",
            Self::RUNNING => "RUNNING",
            Self::RETRYING => "RETRYING",
            Self::SUCCESS => "SUCCESS",
            Self::FAILED => "FAILED",
            Self::CANCELLED => "CANCELLED",
            Self::WAITING => "WAITING",
            Self::POLLING => "POLLING",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Execution {
    pub execution_id: String,
    pub job_id: String,
    pub endpoint: String,
    pub endpoint_type: String,
    pub idempotency_key: Option<String>,
    pub status: String,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub worker_id: Option<String>,
    pub run_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub poll_url: Option<String>,
    pub poll_count: i32,
    pub polling_started_at: Option<DateTime<Utc>>,
    pub polling_deadline: Option<DateTime<Utc>>,
    pub max_wait_ms: Option<i64>,
    pub max_polls: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_and_polling_render_to_strings() {
        assert_eq!(ExecutionStatus::WAITING.as_str(), "WAITING");
        assert_eq!(ExecutionStatus::POLLING.as_str(), "POLLING");
    }
}
