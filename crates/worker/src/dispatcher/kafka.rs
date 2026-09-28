use super::DispatchResult;
use invokr_common::metrics as m;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub async fn dispatch(spec: &Value) -> DispatchResult {
    let bootstrap_servers = spec["bootstrap_servers"].as_str().unwrap_or_default();
    let topic = spec["topic"].as_str().unwrap_or_default();
    let timeout_ms = spec["timeout_ms"].as_u64().unwrap_or(10000);
    let acks = spec["acks"].as_str().unwrap_or("all");

    let key = spec
        .get("key_template")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let value = spec
        .get("value_template")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_string());

    let producer: FutureProducer = match ClientConfig::new()
        .set("bootstrap.servers", bootstrap_servers)
        .set("message.timeout.ms", timeout_ms.to_string())
        .set("acks", acks)
        .create()
    {
        Ok(p) => p,
        Err(e) => {
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "KAFKA",
                "status" => "FAILURE",
                "error_type" => "BROKER_ERROR",
            )
            .increment(1);
            return DispatchResult::Failure {
                error: serde_json::json!({
                    "type": "BROKER_ERROR",
                    "message": format!("Failed to create producer: {}", e),
                }),
            };
        }
    };

    let mut record = FutureRecord::to(topic).payload(&value);
    if let Some(ref k) = key {
        record = record.key(k);
    }

    // Set headers if present
    if let Some(headers_obj) = spec.get("headers").and_then(|h| h.as_object()) {
        let mut headers = rdkafka::message::OwnedHeaders::new();
        for (k, v) in headers_obj {
            if let Some(val) = v.as_str() {
                headers = headers.insert(rdkafka::message::Header {
                    key: k,
                    value: Some(val),
                });
            }
        }
        record = record.headers(headers);
    }

    let start = std::time::Instant::now();

    match producer
        .send(record, Duration::from_millis(timeout_ms))
        .await
    {
        Ok((partition, offset)) => {
            let elapsed = start.elapsed().as_secs_f64();
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "KAFKA",
                "status" => "SUCCESS",
                "error_type" => "",
            )
            .increment(1);
            metrics::counter!(m::KAFKA_MESSAGES_PRODUCED_TOTAL,
                "topic" => topic.to_string(),
                "status" => "SUCCESS",
            )
            .increment(1);
            metrics::histogram!(m::DISPATCH_DURATION_SECONDS,
                "endpoint_type" => "KAFKA",
            )
            .record(elapsed);

            DispatchResult::Success {
                output: serde_json::json!({
                    "partition": partition,
                    "offset": offset,
                }),
                headers: HashMap::new(),
                status_code: 0,
            }
        }
        Err((e, _)) => {
            let error_type = if e.to_string().contains("timeout") {
                "TIMEOUT"
            } else {
                "BROKER_ERROR"
            };
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "KAFKA",
                "status" => "FAILURE",
                "error_type" => error_type.to_string(),
            )
            .increment(1);
            metrics::counter!(m::KAFKA_MESSAGES_PRODUCED_TOTAL,
                "topic" => topic.to_string(),
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

    fn kafka_url() -> String {
        std::env::var("TEST_KAFKA_BOOTSTRAP").unwrap_or_else(|_| "localhost:9092".into())
    }

    /// Produce a message to Kafka and verify partition/offset are returned.
    /// Requires: `docker compose --profile kafka up -d`
    #[tokio::test]
    async fn test_kafka_dispatch_success() {
        let topic = format!("invokr_test_{}", uuid::Uuid::new_v4().simple());
        let spec = json!({
            "bootstrap_servers": kafka_url(),
            "topic": topic,
            "value_template": json!({"hello": "world"}),
            "timeout_ms": 15000,
        });

        let result = dispatch(&spec).await;
        assert!(result.is_success(), "expected success, got failure");
        if let DispatchResult::Success { output, .. } = result {
            assert!(output.get("partition").is_some());
            assert!(output.get("offset").is_some());
        }
    }

    /// Produce a message with a key and custom headers.
    #[tokio::test]
    async fn test_kafka_dispatch_with_key_and_headers() {
        let topic = format!("invokr_test_{}", uuid::Uuid::new_v4().simple());
        let spec = json!({
            "bootstrap_servers": kafka_url(),
            "topic": topic,
            "key_template": "my-key-123",
            "value_template": json!({"event": "test"}),
            "headers": {
                "X-Source": "invokr-test",
                "X-Trace-Id": "abc-123",
            },
            "timeout_ms": 15000,
        });

        let result = dispatch(&spec).await;
        assert!(result.is_success(), "expected success with key+headers");
    }

    /// Sending to a bad broker address should fail with BROKER_ERROR or TIMEOUT.
    #[tokio::test]
    async fn test_kafka_dispatch_bad_broker() {
        let spec = json!({
            "bootstrap_servers": "localhost:19999",
            "topic": "nonexistent",
            "value_template": json!({"x": 1}),
            "timeout_ms": 3000,
        });

        let result = dispatch(&spec).await;
        assert!(result.is_failure(), "expected failure for bad broker");
        if let DispatchResult::Failure { error } = result {
            let err_type = error["type"].as_str().unwrap();
            assert!(
                err_type == "BROKER_ERROR" || err_type == "TIMEOUT",
                "unexpected error type: {}",
                err_type
            );
        }
    }

    /// Multiple sequential produces should all succeed and have increasing offsets.
    #[tokio::test]
    async fn test_kafka_dispatch_multiple_messages() {
        let topic = format!("invokr_test_{}", uuid::Uuid::new_v4().simple());
        let mut offsets = vec![];

        for i in 0..3 {
            let spec = json!({
                "bootstrap_servers": kafka_url(),
                "topic": topic,
                "value_template": json!({"seq": i}),
                "timeout_ms": 15000,
            });

            let result = dispatch(&spec).await;
            assert!(result.is_success(), "message {} failed", i);
            if let DispatchResult::Success { output, .. } = result {
                offsets.push(output["offset"].as_i64().unwrap());
            }
        }

        // Offsets should be monotonically increasing
        for window in offsets.windows(2) {
            assert!(
                window[1] > window[0],
                "offsets not increasing: {:?}",
                offsets
            );
        }
    }
}
