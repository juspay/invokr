use super::DispatchResult;
use invokr_common::metrics as m;
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Dispatch an HTTP request.
///
/// `async_status_codes` are the endpoint's `async.status_codes` — the codes that
/// mean "accepted, still working". They are treated as a successful dispatch so
/// the caller can inspect `status_code` / `headers` and park the execution in
/// WAITING. Endpoint validation requires them to be disjoint from
/// `expected_status_codes`, so without this they would land in the failure arm
/// and the long-running path would be unreachable. Pass `&[]` for endpoints with
/// no async block.
pub async fn dispatch(
    client: &Client,
    spec: &Value,
    idempotency_key: &str,
    async_status_codes: &[u16],
) -> DispatchResult {
    let url = spec["url"].as_str().unwrap_or_default();
    let method = spec["method"].as_str().unwrap_or("POST");
    let timeout_ms = spec["timeout_ms"].as_u64().unwrap_or(5000);
    let expected_statuses: Vec<u16> = spec["expected_status_codes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as u16))
                .collect()
        })
        .unwrap_or_else(|| vec![200, 201, 202, 204]);

    let mut req = match method.to_uppercase().as_str() {
        "GET" => client.get(url),
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "PATCH" => client.patch(url),
        "DELETE" => client.delete(url),
        _ => client.post(url),
    };

    let mut has_user_idempotency_header = false;

    // Set headers
    if let Some(headers) = spec["headers"].as_object() {
        for (k, v) in headers {
            if let Some(val) = v.as_str() {
                req = req.header(k.as_str(), val);
                if k.eq_ignore_ascii_case("x-invokr-idempotency-key") {
                    has_user_idempotency_header = true;
                }
            }
        }
    }

    if !has_user_idempotency_header {
        req = req.header("x-invokr-idempotency-key", idempotency_key);
    }

    req = req.timeout(Duration::from_millis(timeout_ms));

    // Set body: use body_template if present, otherwise use body, otherwise send empty JSON object
    if let Some(body) = spec.get("body_template").or_else(|| spec.get("body")) {
        if !body.is_null() {
            req = req.json(body);
        }
    }

    let start = std::time::Instant::now();

    match req.send().await {
        Ok(response) => {
            let elapsed = start.elapsed().as_secs_f64();
            let status = response.status().as_u16();
            let resp_headers: HashMap<String, String> = response
                .headers()
                .iter()
                .filter_map(|(k, v)| {
                    v.to_str()
                        .ok()
                        .map(|s| (k.as_str().to_ascii_lowercase(), s.to_string()))
                })
                .collect();
            let body = response.text().await.unwrap_or_default();

            if expected_statuses.contains(&status) || async_status_codes.contains(&status) {
                metrics::counter!(m::DISPATCH_TOTAL,
                    "endpoint_type" => "HTTP",
                    "status" => "SUCCESS",
                    "error_type" => "",
                )
                .increment(1);
                metrics::histogram!(m::DISPATCH_DURATION_SECONDS,
                    "endpoint_type" => "HTTP",
                )
                .record(elapsed);

                DispatchResult::Success {
                    output: serde_json::json!({
                        "status_code": status,
                        "body": body,
                    }),
                    headers: resp_headers,
                    status_code: status,
                }
            } else {
                metrics::counter!(m::DISPATCH_TOTAL,
                    "endpoint_type" => "HTTP",
                    "status" => "FAILURE",
                    "error_type" => "HTTP_ERROR",
                )
                .increment(1);

                DispatchResult::Failure {
                    error: serde_json::json!({
                        "type": "HTTP_ERROR",
                        "status_code": status,
                        "message": format!("Unexpected status code: {}", status),
                    }),
                }
            }
        }
        Err(e) => {
            let error_type = if e.is_timeout() {
                "TIMEOUT"
            } else if e.is_connect() {
                "CONNECTION_ERROR"
            } else {
                "HTTP_ERROR"
            };

            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "HTTP",
                "status" => "FAILURE",
                "error_type" => error_type.to_string(),
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

    fn mock_url() -> String {
        std::env::var("MOCK_URL").unwrap_or_else(|_| "http://localhost:9999".into())
    }

    /// POST to mock-server /success and verify 200 response.
    /// Requires: `cargo run -p invokr-mock-server`
    #[tokio::test]
    async fn test_http_dispatch_success() {
        let client = Client::new();
        let spec = json!({
            "url": format!("{}/success", mock_url()),
            "method": "POST",
            "headers": {
                "Content-Type": "application/json",
            },
            "body": {"test": true},
            "timeout_ms": 5000,
        });

        let result = dispatch(&client, &spec, "test-http-dispatch-success", &[]).await;
        assert!(result.is_success(), "expected success from /success");
        if let DispatchResult::Success { output, .. } = result {
            assert_eq!(output["status_code"].as_u64().unwrap(), 200);
        }
    }

    /// Connection to a non-existent server should fail with CONNECTION_ERROR.
    #[tokio::test]
    async fn test_http_dispatch_connection_error() {
        let client = Client::new();
        let spec = json!({
            "url": "http://127.0.0.1:19999/nothing",
            "method": "POST",
            "timeout_ms": 2000,
        });

        let result = dispatch(&client, &spec, "test-http-dispatch-connection-error", &[]).await;
        assert!(result.is_failure(), "expected connection failure");
        if let DispatchResult::Failure { error } = result {
            let err_type = error["type"].as_str().unwrap();
            assert!(
                err_type == "CONNECTION_ERROR" || err_type == "TIMEOUT",
                "unexpected error type: {}",
                err_type
            );
        }
    }

    /// A 500 response should be treated as a failure (not in expected_status_codes).
    #[tokio::test]
    async fn test_http_dispatch_unexpected_status() {
        let client = Client::new();
        let spec = json!({
            "url": format!("{}/fail", mock_url()),
            "method": "POST",
            "expected_status_codes": [200],
            "timeout_ms": 5000,
        });

        let result = dispatch(&client, &spec, "test-http-dispatch-unexpected-status", &[]).await;
        // If mock-server returns 500 for /fail, this should be a failure
        if result.is_failure() {
            if let DispatchResult::Failure { error } = result {
                assert_eq!(error["type"].as_str().unwrap(), "HTTP_ERROR");
            }
        }
    }

    /// GET request should work.
    #[tokio::test]
    async fn test_http_dispatch_get_method() {
        let client = Client::new();
        let spec = json!({
            "url": format!("{}/health", mock_url()),
            "method": "GET",
            "expected_status_codes": [200],
            "timeout_ms": 5000,
        });

        let result = dispatch(&client, &spec, "test-http-dispatch-get-method", &[]).await;
        assert!(result.is_success(), "expected success from GET /health");
    }

    /// Custom headers should be sent.
    #[tokio::test]
    async fn test_http_dispatch_with_headers() {
        let client = Client::new();
        let spec = json!({
            "url": format!("{}/success", mock_url()),
            "method": "POST",
            "headers": {
                "X-Custom-Header": "test-value",
                "Authorization": "Bearer test-token",
            },
            "body": {"data": "with headers"},
            "timeout_ms": 5000,
        });

        let result = dispatch(&client, &spec, "test-http-dispatch-with-headers", &[]).await;
        assert!(result.is_success(), "expected success with custom headers");
    }

    fn async_spec() -> Value {
        // Endpoint validation requires async.status_codes and expected_status_codes
        // to be disjoint, so 202 is deliberately absent from expected_status_codes.
        json!({
            "url": format!("{}/async/start", mock_url()),
            "method": "POST",
            "expected_status_codes": [200],
            "body": {"script": [{"status": 202}, {"status": 200}]},
            "timeout_ms": 5000,
        })
    }

    /// An async status code must yield Success so the pipeline can read the
    /// Location header and park the execution in WAITING.
    #[tokio::test]
    async fn test_async_status_code_is_success() {
        let client = Client::new();
        let result = dispatch(&client, &async_spec(), "test-http-async-success", &[202]).await;

        match result {
            DispatchResult::Success {
                status_code,
                headers,
                ..
            } => {
                assert_eq!(status_code, 202);
                assert!(
                    headers.contains_key("location"),
                    "Location header missing; got {:?}",
                    headers.keys().collect::<Vec<_>>()
                );
            }
            DispatchResult::Failure { error } => {
                panic!("202 should dispatch successfully when declared async: {error}")
            }
        }
    }

    /// Same response, but the endpoint declares no async codes — still a failure,
    /// since 202 is not in expected_status_codes.
    #[tokio::test]
    async fn test_async_status_code_without_async_config_is_failure() {
        let client = Client::new();
        let result = dispatch(&client, &async_spec(), "test-http-async-not-declared", &[]).await;

        assert!(
            result.is_failure(),
            "202 must stay a failure when the endpoint has no async block"
        );
    }
}
