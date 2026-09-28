use super::DispatchResult;
use invokr_common::metrics as m;
use serde_json::Value;
use std::collections::HashMap;

pub async fn dispatch(spec: &Value) -> DispatchResult {
    let redis_url = spec["redis_url"]
        .as_str()
        .unwrap_or("redis://127.0.0.1:6379");
    let stream = spec["stream"].as_str().unwrap_or_default();
    let max_len = spec.get("max_len").and_then(|v| v.as_u64());
    let approximate = spec
        .get("approximate_trimming")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let fields_template = match spec.get("fields_template").and_then(|v| v.as_object()) {
        Some(f) => f,
        None => {
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "REDIS_STREAM",
                "status" => "FAILURE",
                "error_type" => "STREAM_ERROR",
            )
            .increment(1);
            return DispatchResult::Failure {
                error: serde_json::json!({
                    "type": "STREAM_ERROR",
                    "message": "Missing fields_template in spec",
                }),
            };
        }
    };

    let client = match redis::Client::open(redis_url) {
        Ok(c) => c,
        Err(e) => {
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "REDIS_STREAM",
                "status" => "FAILURE",
                "error_type" => "CONNECTION_ERROR",
            )
            .increment(1);
            return DispatchResult::Failure {
                error: serde_json::json!({
                    "type": "CONNECTION_ERROR",
                    "message": format!("Failed to connect to Redis: {}", e),
                }),
            };
        }
    };

    let mut conn = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(e) => {
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "REDIS_STREAM",
                "status" => "FAILURE",
                "error_type" => "CONNECTION_ERROR",
            )
            .increment(1);
            return DispatchResult::Failure {
                error: serde_json::json!({
                    "type": "CONNECTION_ERROR",
                    "message": format!("Redis connection failed: {}", e),
                }),
            };
        }
    };

    // Build field pairs for XADD
    let fields: Vec<(String, String)> = fields_template
        .iter()
        .map(|(k, v)| {
            let val = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), val)
        })
        .collect();

    let field_refs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let start = std::time::Instant::now();

    // XADD with optional MAXLEN trimming
    let result: Result<String, redis::RedisError> = if let Some(maxlen) = max_len {
        if approximate {
            redis::cmd("XADD")
                .arg(stream)
                .arg("MAXLEN")
                .arg("~")
                .arg(maxlen)
                .arg("*")
                .arg(&field_refs)
                .query_async(&mut conn)
                .await
        } else {
            redis::cmd("XADD")
                .arg(stream)
                .arg("MAXLEN")
                .arg(maxlen)
                .arg("*")
                .arg(&field_refs)
                .query_async(&mut conn)
                .await
        }
    } else {
        redis::cmd("XADD")
            .arg(stream)
            .arg("*")
            .arg(&field_refs)
            .query_async(&mut conn)
            .await
    };

    match result {
        Ok(message_id) => {
            let elapsed = start.elapsed().as_secs_f64();
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "REDIS_STREAM",
                "status" => "SUCCESS",
                "error_type" => "",
            )
            .increment(1);
            metrics::counter!(m::REDIS_STREAM_MESSAGES_SENT_TOTAL,
                "stream" => stream.to_string(),
                "status" => "SUCCESS",
            )
            .increment(1);
            metrics::histogram!(m::DISPATCH_DURATION_SECONDS,
                "endpoint_type" => "REDIS_STREAM",
            )
            .record(elapsed);

            DispatchResult::Success {
                output: serde_json::json!({
                    "message_id": message_id,
                    "stream": stream,
                }),
                headers: HashMap::new(),
                status_code: 0,
            }
        }
        Err(e) => {
            let error_type = if e.to_string().contains("timeout") {
                "TIMEOUT"
            } else {
                "STREAM_ERROR"
            };
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "REDIS_STREAM",
                "status" => "FAILURE",
                "error_type" => error_type.to_string(),
            )
            .increment(1);
            metrics::counter!(m::REDIS_STREAM_MESSAGES_SENT_TOTAL,
                "stream" => stream.to_string(),
                "status" => "FAILURE",
            )
            .increment(1);

            DispatchResult::Failure {
                error: serde_json::json!({
                    "type": error_type,
                    "message": e.to_string(),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn redis_url() -> String {
        std::env::var("TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into())
    }

    /// XADD a message and verify message_id is returned.
    /// Requires: `docker compose --profile redis up -d`
    #[tokio::test]
    async fn test_redis_stream_dispatch_success() {
        let stream = format!("invokr_test:{}", uuid::Uuid::new_v4().simple());
        let spec = json!({
            "redis_url": redis_url(),
            "stream": stream,
            "fields_template": {
                "event": "job.executed",
                "payload": "hello world",
            },
        });

        let result = dispatch(&spec).await;
        assert!(result.is_success(), "expected success");
        if let DispatchResult::Success { output, .. } = result {
            let msg_id = output["message_id"].as_str().unwrap();
            // Redis stream IDs look like "1234567890123-0"
            assert!(msg_id.contains('-'), "invalid message_id: {}", msg_id);
            assert_eq!(output["stream"].as_str().unwrap(), stream);
        }
    }

    /// XADD with MAXLEN trimming.
    #[tokio::test]
    async fn test_redis_stream_dispatch_with_maxlen() {
        let stream = format!("invokr_test:{}", uuid::Uuid::new_v4().simple());
        let spec = json!({
            "redis_url": redis_url(),
            "stream": stream,
            "fields_template": {
                "event": "trimmed",
            },
            "max_len": 100,
            "approximate_trimming": true,
        });

        let result = dispatch(&spec).await;
        assert!(result.is_success(), "expected success with maxlen");
    }

    /// XADD with exact trimming (approximate_trimming = false).
    #[tokio::test]
    async fn test_redis_stream_dispatch_exact_trimming() {
        let stream = format!("invokr_test:{}", uuid::Uuid::new_v4().simple());
        let spec = json!({
            "redis_url": redis_url(),
            "stream": stream,
            "fields_template": {
                "event": "exact_trim",
            },
            "max_len": 50,
            "approximate_trimming": false,
        });

        let result = dispatch(&spec).await;
        assert!(result.is_success(), "expected success with exact trimming");
    }

    /// Missing fields_template should fail with STREAM_ERROR.
    #[tokio::test]
    async fn test_redis_stream_dispatch_missing_fields() {
        let spec = json!({
            "redis_url": redis_url(),
            "stream": "test-stream",
        });

        let result = dispatch(&spec).await;
        assert!(result.is_failure(), "expected failure for missing fields");
        if let DispatchResult::Failure { error } = result {
            assert_eq!(error["type"].as_str().unwrap(), "STREAM_ERROR");
            assert!(error["message"]
                .as_str()
                .unwrap()
                .contains("fields_template"));
        }
    }

    /// Connecting to a bad Redis URL should fail with CONNECTION_ERROR.
    #[tokio::test]
    async fn test_redis_stream_dispatch_bad_connection() {
        let spec = json!({
            "redis_url": "redis://127.0.0.1:19999",
            "stream": "test-stream",
            "fields_template": {
                "event": "should_fail",
            },
        });

        let result = dispatch(&spec).await;
        assert!(result.is_failure(), "expected failure for bad redis url");
        if let DispatchResult::Failure { error } = result {
            assert_eq!(error["type"].as_str().unwrap(), "CONNECTION_ERROR");
        }
    }

    /// Multiple messages to the same stream should each get unique IDs.
    #[tokio::test]
    async fn test_redis_stream_dispatch_multiple_messages() {
        let stream = format!("invokr_test:{}", uuid::Uuid::new_v4().simple());
        let mut message_ids = vec![];

        for i in 0..3 {
            let spec = json!({
                "redis_url": redis_url(),
                "stream": stream,
                "fields_template": {
                    "seq": i.to_string(),
                    "event": "batch_test",
                },
            });

            let result = dispatch(&spec).await;
            assert!(result.is_success(), "message {} failed", i);
            if let DispatchResult::Success { output, .. } = result {
                message_ids.push(output["message_id"].as_str().unwrap().to_string());
            }
        }

        // All IDs should be unique
        let unique: std::collections::HashSet<_> = message_ids.iter().collect();
        assert_eq!(unique.len(), 3, "expected 3 unique message IDs");
    }

    /// Non-string field values should be serialized to JSON strings.
    #[tokio::test]
    async fn test_redis_stream_dispatch_mixed_field_types() {
        let stream = format!("invokr_test:{}", uuid::Uuid::new_v4().simple());
        let spec = json!({
            "redis_url": redis_url(),
            "stream": stream,
            "fields_template": {
                "string_field": "hello",
                "number_field": 42,
                "bool_field": true,
                "object_field": {"nested": "value"},
            },
        });

        let result = dispatch(&spec).await;
        assert!(result.is_success(), "expected success with mixed types");
    }
}
