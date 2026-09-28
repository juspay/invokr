pub mod http;
pub mod internal;
#[cfg(feature = "kafka")]
pub mod kafka;
#[cfg(feature = "redis-stream")]
pub mod redis_stream;

use serde_json::Value;
use std::collections::HashMap;

pub enum DispatchResult {
    Success {
        output: Value,
        headers: HashMap<String, String>,
        status_code: u16,
    },
    Failure {
        error: Value,
    },
}

#[cfg(test)]
impl DispatchResult {
    pub fn is_success(&self) -> bool {
        matches!(self, DispatchResult::Success { .. })
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, DispatchResult::Failure { .. })
    }
}
