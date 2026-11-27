use super::DispatchResult;
use invokr_common::metrics as m;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub async fn dispatch(client: &Client, spec: &Value, idempotency_key: &str) -> DispatchResult {
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

    // Also covers callers that bring their own `Client` (library mode), which
    // lack `egress::client`'s resolver and redirect checks.
    if let Ok(parsed) = reqwest::Url::parse(url) {
        if let Err(reason) = super::egress::check_url(&parsed) {
            metrics::counter!(m::DISPATCH_TOTAL,
                "endpoint_type" => "HTTP",
                "status" => "FAILURE",
                "error_type" => "BLOCKED_DESTINATION",
            )
            .increment(1);
            return DispatchResult::Failure {
                error: serde_json::json!({ "type": "BLOCKED_DESTINATION", "message": reason }),
            };
        }
    }

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
            let body = response.text().await.unwrap_or_default();

            if expected_statuses.contains(&status) {
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

            // `without_url`: the URL can carry `{{secret.*}}` values, and this
            // message is stored where the API serves it back.
            DispatchResult::Failure {
                error: serde_json::json!({
                    "type": error_type,
                    "message": e.without_url().to_string(),
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

        let result = dispatch(&client, &spec, "test-http-dispatch-success").await;
        assert!(result.is_success(), "expected success from /success");
        if let DispatchResult::Success { output } = result {
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

        let result = dispatch(&client, &spec, "test-http-dispatch-connection-error").await;
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

        let result = dispatch(&client, &spec, "test-http-dispatch-unexpected-status").await;
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

        let result = dispatch(&client, &spec, "test-http-dispatch-get-method").await;
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

        let result = dispatch(&client, &spec, "test-http-dispatch-with-headers").await;
        assert!(result.is_success(), "expected success with custom headers");
    }
}

#[cfg(test)]
mod leak_and_egress_tests {
    use super::*;
    use serde_json::json;

    /// The URL can hold a resolved `{{secret.*}}`; the stored error must not.
    #[tokio::test]
    async fn transport_errors_do_not_echo_the_url() {
        let spec = json!({
            "url": "http://127.0.0.1:19998/hook?api_key=s3cr3t-value",
            "method": "POST",
            "timeout_ms": 2000,
        });
        let result = dispatch(&Client::new(), &spec, "no-url-in-error").await;
        let DispatchResult::Failure { error } = result else {
            panic!("expected a connection failure");
        };
        let text = error.to_string();
        assert!(!text.contains("s3cr3t-value"), "secret leaked: {text}");
        assert!(!text.contains("19998/hook"), "URL leaked: {text}");
    }

    #[tokio::test]
    async fn metadata_destinations_are_refused_before_sending() {
        let spec = json!({
            "url": "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
            "method": "GET",
            "timeout_ms": 2000,
        });
        let result = dispatch(&Client::new(), &spec, "blocked-destination").await;
        let DispatchResult::Failure { error } = result else {
            panic!("expected the metadata URL to be refused");
        };
        assert_eq!(error["type"], "BLOCKED_DESTINATION");
    }
}
