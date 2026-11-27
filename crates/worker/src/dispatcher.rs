pub mod egress;
pub mod http;
pub mod internal;
#[cfg(feature = "kafka")]
pub mod kafka;
#[cfg(feature = "redis-stream")]
pub mod redis_stream;

use serde_json::Value;

pub enum DispatchResult {
    Success { output: Value },
    Failure { error: Value },
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
