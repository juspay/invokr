use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum PollClassification {
    SUCCESS,
    PENDING,
    TERMINAL_FAILURE,
    TRANSIENT_ERROR,
}

impl PollClassification {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SUCCESS => "SUCCESS",
            Self::PENDING => "PENDING",
            Self::TERMINAL_FAILURE => "TERMINAL_FAILURE",
            Self::TRANSIENT_ERROR => "TRANSIENT_ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Poll {
    pub execution_id: String,
    pub poll_number: i32,
    pub polled_at: DateTime<Utc>,
    pub duration_ms: Option<i64>,
    pub status_code: Option<i32>,
    pub retry_after_ms: Option<i64>,
    pub classification: String,
    pub error: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_strings() {
        assert_eq!(PollClassification::SUCCESS.as_str(), "SUCCESS");
        assert_eq!(PollClassification::PENDING.as_str(), "PENDING");
        assert_eq!(
            PollClassification::TERMINAL_FAILURE.as_str(),
            "TERMINAL_FAILURE"
        );
        assert_eq!(
            PollClassification::TRANSIENT_ERROR.as_str(),
            "TRANSIENT_ERROR"
        );
    }
}
