use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[allow(non_camel_case_types)]
pub enum EndpointType {
    HTTP,
    KAFKA,
    REDIS_STREAM,
    /// In-process task run by the worker itself (e.g. the dogfooded CRON reaper).
    /// Not user-creatable — provisioned at workspace-creation time for internal
    /// jobs whose "dispatch" is a Rust function rather than a network call.
    INTERNAL,
}

impl fmt::Display for EndpointType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl EndpointType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HTTP => "HTTP",
            Self::KAFKA => "KAFKA",
            Self::REDIS_STREAM => "REDIS_STREAM",
            Self::INTERNAL => "INTERNAL",
        }
    }

    pub fn from_str_val(s: &str) -> Option<Self> {
        match s {
            "HTTP" => Some(Self::HTTP),
            "KAFKA" => Some(Self::KAFKA),
            "REDIS_STREAM" => Some(Self::REDIS_STREAM),
            "INTERNAL" => Some(Self::INTERNAL),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: i64,
    #[serde(default = "default_backoff")]
    pub backoff: String,
    #[serde(default = "default_initial_delay")]
    pub initial_delay_ms: i64,
    #[serde(default = "default_max_delay")]
    pub max_delay_ms: i64,
}

fn default_max_attempts() -> i64 {
    1
}
fn default_backoff() -> String {
    "exponential".into()
}
fn default_initial_delay() -> i64 {
    1000
}
fn default_max_delay() -> i64 {
    60000
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            backoff: default_backoff(),
            initial_delay_ms: default_initial_delay(),
            max_delay_ms: default_max_delay(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Endpoint {
    pub name: String,
    pub endpoint_type: String,
    pub payload_spec_ref: Option<String>,
    pub config_ref: Option<String>,
    pub spec: serde_json::Value,
    pub retry_policy: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Endpoint {
    pub fn get_retry_policy(&self) -> RetryPolicy {
        self.retry_policy
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    pub fn get_async_config(&self) -> Option<AsyncConfig> {
        self.spec
            .get("async")
            .and_then(|v| serde_json::from_value::<AsyncConfig>(v.clone()).ok())
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateEndpoint {
    pub name: String,
    #[serde(rename = "type")]
    pub endpoint_type: String,
    pub payload_spec: Option<String>,
    pub config: Option<String>,
    pub spec: serde_json::Value,
    pub retry_policy: Option<RetryPolicy>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEndpoint {
    pub spec: Option<serde_json::Value>,
    pub config: Option<String>,
    pub payload_spec: Option<String>,
    pub retry_policy: Option<RetryPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncConfig {
    #[serde(default = "default_async_status_codes")]
    pub status_codes: Vec<u16>,
    pub poll: Option<PollConfig>,
    #[serde(default)]
    pub callback: bool,
    #[serde(default = "default_max_wait_ms")]
    pub max_wait_ms: i64,
    #[serde(default = "default_max_polls")]
    pub max_polls: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollConfig {
    #[serde(default = "default_success_statuses")]
    pub success_statuses: Vec<u16>,
    #[serde(default = "default_pending_statuses")]
    pub pending_statuses: Vec<u16>,
    #[serde(default = "default_failure_statuses")]
    pub failure_statuses: Vec<u16>,
    #[serde(default = "default_poll_initial_delay")]
    pub initial_delay_ms: i64,
    #[serde(default = "default_poll_max_delay")]
    pub max_delay_ms: i64,
    #[serde(default = "default_poll_backoff")]
    pub backoff: String,
    /// Re-dispatch after this many consecutive TRANSIENT_ERROR polls. Any other
    /// outcome resets the run. Explicit `null` disables the cap and polls until
    /// the wait/poll budget runs out.
    #[serde(default = "default_max_consecutive_errors")]
    pub max_consecutive_errors: Option<i32>,
}

fn default_async_status_codes() -> Vec<u16> {
    vec![202]
}
fn default_success_statuses() -> Vec<u16> {
    vec![200]
}
fn default_pending_statuses() -> Vec<u16> {
    vec![202]
}
/// Codes that will not resolve themselves on a retry. Anything else — 429, 408,
/// 503 — stays transient so rate limiting and restarts are tolerated.
fn default_failure_statuses() -> Vec<u16> {
    vec![400, 401, 403, 404, 410]
}
fn default_max_wait_ms() -> i64 {
    3_600_000
}
fn default_max_polls() -> i32 {
    1_000
}
fn default_poll_initial_delay() -> i64 {
    1_000
}
fn default_poll_max_delay() -> i64 {
    60_000
}
fn default_poll_backoff() -> String {
    "exponential".into()
}
fn default_max_consecutive_errors() -> Option<i32> {
    Some(10)
}

pub fn validate_async_block(spec: &serde_json::Value) -> Result<(), String> {
    let Some(async_val) = spec.get("async") else {
        return Ok(());
    };

    let cfg: AsyncConfig = serde_json::from_value(async_val.clone())
        .map_err(|e| format!("invalid async config: {e}"))?;

    if cfg.poll.is_none() && !cfg.callback {
        return Err("async block must enable at least one of poll or callback".into());
    }

    let expected: std::collections::HashSet<u16> = spec
        .get("expected_status_codes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as u16))
                .collect()
        })
        .unwrap_or_default();
    let initial: std::collections::HashSet<u16> = cfg.status_codes.iter().copied().collect();
    if !expected.is_disjoint(&initial) {
        return Err(format!(
            "async.status_codes and expected_status_codes must be disjoint (overlap: {:?})",
            expected.intersection(&initial).collect::<Vec<_>>()
        ));
    }

    if cfg.max_wait_ms < 1 || cfg.max_wait_ms > 30 * 24 * 3600 * 1000 {
        return Err("async.max_wait_ms out of range (1 .. 30d)".into());
    }
    if cfg.max_polls < 1 || cfg.max_polls > 100_000 {
        return Err("async.max_polls out of range (1 .. 100000)".into());
    }

    if let Some(p) = &cfg.poll {
        let succ: std::collections::HashSet<u16> = p.success_statuses.iter().copied().collect();
        let pend: std::collections::HashSet<u16> = p.pending_statuses.iter().copied().collect();
        let fail: std::collections::HashSet<u16> = p.failure_statuses.iter().copied().collect();

        // intra-set duplicates
        if succ.len() != p.success_statuses.len()
            || pend.len() != p.pending_statuses.len()
            || fail.len() != p.failure_statuses.len()
        {
            return Err("async.poll status sets contain duplicates".into());
        }
        if !succ.is_disjoint(&pend) || !succ.is_disjoint(&fail) || !pend.is_disjoint(&fail) {
            return Err("async.poll status sets must be pairwise disjoint".into());
        }
        if p.initial_delay_ms < 1 || p.max_delay_ms < p.initial_delay_ms {
            return Err("async.poll initial_delay_ms / max_delay_ms invalid".into());
        }
        if p.max_consecutive_errors.is_some_and(|n| n < 1) {
            return Err("async.poll.max_consecutive_errors must be >= 1".into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod async_validation_tests {
    pub fn validate_async(spec: &serde_json::Value) -> Result<(), String> {
        crate::models::endpoint::validate_async_block(spec)
    }

    #[test]
    fn rejects_overlapping_initial_status_codes() {
        let spec = serde_json::json!({
            "expected_status_codes": [200, 202],
            "async": {
                "status_codes": [202],
                "poll": {"success_statuses":[200],"pending_statuses":[202],"failure_statuses":[]},
                "max_wait_ms": 60000,
                "max_polls": 10
            }
        });
        let err = validate_async(&spec).unwrap_err();
        assert!(err.contains("disjoint"), "got: {}", err);
    }

    #[test]
    fn rejects_overlapping_poll_status_sets() {
        let spec = serde_json::json!({
            "expected_status_codes": [200],
            "async": {
                "status_codes": [202],
                "poll": {"success_statuses":[200,200],"pending_statuses":[200],"failure_statuses":[]},
                "max_wait_ms": 60000,
                "max_polls": 10
            }
        });
        assert!(validate_async(&spec).is_err());
    }

    #[test]
    fn rejects_both_modes_off() {
        let spec = serde_json::json!({
            "expected_status_codes": [200],
            "async": {"status_codes":[202],"max_wait_ms":60000,"max_polls":10}
        });
        assert!(validate_async(&spec).is_err());
    }

    #[test]
    fn accepts_minimal_valid_polling_only() {
        let spec = serde_json::json!({
            "expected_status_codes": [200],
            "async": {
                "status_codes": [202],
                "poll": {"success_statuses":[200],"pending_statuses":[202],"failure_statuses":[]},
                "max_wait_ms": 60000,
                "max_polls": 10
            }
        });
        assert!(validate_async(&spec).is_ok());
    }

    #[test]
    fn accepts_callback_only() {
        let spec = serde_json::json!({
            "expected_status_codes": [200],
            "async": {
                "status_codes": [202],
                "callback": true,
                "max_wait_ms": 60000,
                "max_polls": 10
            }
        });
        assert!(validate_async(&spec).is_ok());
    }

    #[test]
    fn accepts_no_async_block() {
        let spec = serde_json::json!({ "expected_status_codes": [200] });
        assert!(validate_async(&spec).is_ok());
    }
}
