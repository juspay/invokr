//! Long-running job polling utilities. Helpers live here so they can be reused
//! by both initial dispatch (Task 13) and the poll path (Task 15).

/// Resolve a possibly relative URL against a base URL.
/// Absolute URLs (http:// or https://) pass through unchanged.
pub fn resolve_relative_url(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let Ok(base_url) = url::Url::parse(base) else {
        return location.to_string();
    };
    base_url
        .join(location)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| location.to_string())
}

/// Parse a Retry-After header value (seconds-as-integer or HTTP-date) into milliseconds.
pub fn parse_retry_after(header: Option<&str>) -> Option<i64> {
    let s = header?.trim();
    if let Ok(secs) = s.parse::<i64>() {
        return Some(secs * 1000);
    }
    // Try to parse as an HTTP-date (RFC 7231 / RFC 2822).
    // chrono's rfc2822 parser validates the weekday, so we normalise first:
    //   1. Strip an optional "Weekday, " prefix (3-letter day + ", ").
    //   2. Replace the bare timezone word "GMT" with the numeric "+0000".
    let no_wd = if s.as_bytes().get(3) == Some(&b',') {
        s.get(5..).map(|t| t.trim_start()).unwrap_or(s)
    } else {
        s
    };
    let canonical = no_wd.replace("GMT", "+0000");
    if let Ok(when) = chrono::DateTime::parse_from_str(&canonical, "%d %b %Y %H:%M:%S %z") {
        let now = chrono::Utc::now();
        let delta = when.signed_duration_since(now).num_milliseconds();
        return Some(delta.max(0));
    }
    None
}

use invokr_common::models::PollClassification;

pub fn classify(
    status_code: Option<u16>,
    success: &[u16],
    pending: &[u16],
    failure: &[u16],
) -> PollClassification {
    match status_code {
        Some(c) if success.contains(&c) => PollClassification::SUCCESS,
        Some(c) if failure.contains(&c) => PollClassification::TERMINAL_FAILURE,
        Some(c) if pending.contains(&c) => PollClassification::PENDING,
        _ => PollClassification::TRANSIENT_ERROR,
    }
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    #[test]
    fn success_wins() {
        assert_eq!(
            classify(Some(200), &[200], &[202], &[]),
            PollClassification::SUCCESS
        );
    }
    #[test]
    fn success_beats_failure_on_overlap() {
        // 200 appears in both slices; success arm is checked first.
        assert_eq!(
            classify(Some(200), &[200], &[], &[200]),
            PollClassification::SUCCESS
        );
    }
    #[test]
    fn failure_recognized() {
        assert_eq!(
            classify(Some(410), &[200], &[202], &[410]),
            PollClassification::TERMINAL_FAILURE
        );
    }
    #[test]
    fn pending_recognized() {
        assert_eq!(
            classify(Some(202), &[200], &[202], &[]),
            PollClassification::PENDING
        );
    }
    #[test]
    fn unknown_is_transient() {
        assert_eq!(
            classify(Some(500), &[200], &[202], &[410]),
            PollClassification::TRANSIENT_ERROR
        );
    }
    #[test]
    fn transport_error_is_transient() {
        assert_eq!(
            classify(None, &[200], &[202], &[410]),
            PollClassification::TRANSIENT_ERROR
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_location_passes_through() {
        assert_eq!(
            resolve_relative_url("https://api.example/x", "https://other/y"),
            "https://other/y"
        );
    }

    #[test]
    fn relative_location_resolved_against_base() {
        assert_eq!(
            resolve_relative_url("https://api.example/jobs", "/status/abc"),
            "https://api.example/status/abc"
        );
    }

    #[test]
    fn relative_location_resolved_relative_path() {
        assert_eq!(
            resolve_relative_url("https://api.example/jobs/", "status/abc"),
            "https://api.example/jobs/status/abc"
        );
    }

    #[test]
    fn parse_retry_after_seconds() {
        assert_eq!(parse_retry_after(Some("30")), Some(30_000));
    }

    #[test]
    fn parse_retry_after_http_date_past_returns_zero() {
        let s = "Wed, 01 Jan 1970 00:00:00 GMT";
        assert_eq!(parse_retry_after(Some(s)), Some(0));
    }

    #[test]
    fn parse_retry_after_invalid_returns_none() {
        assert_eq!(parse_retry_after(Some("not a number")), None);
        assert_eq!(parse_retry_after(None), None);
    }
}

use chrono::{Duration, Utc};
use invokr_common::models::endpoint::PollConfig;
use invokr_common::{db, db::DbContext, metrics as m, secrets};
use std::collections::HashMap;

use crate::backoff;
use crate::pipeline::PipelineContext;

pub async fn process_poll(
    ctx: &PipelineContext,
    db: &mut DbContext<'_>,
    schema_name: &str,
    exec: &invokr_common::db::executions::ClaimedExecution,
) {
    let execution_id = &exec.execution_id;
    let attempt_count = exec.attempt_count;
    let _ = schema_name; // currently unused but reserved for future per-schema metrics

    let Some(poll_url) = exec.poll_url.clone() else {
        tracing::error!(execution_id, "POLLING claim has no poll_url");
        let _ = db::executions::complete_failed(db, execution_id).await;
        return;
    };
    let max_polls = exec.max_polls.unwrap_or(1000);
    let deadline = exec.polling_deadline.unwrap_or_else(|| {
        tracing::warn!(execution_id = %execution_id,
            "polling_deadline is NULL on POLLING claim; will TIMEOUT immediately");
        Utc::now()
    });

    // Bound check — pre-network
    if exec.poll_count > max_polls || Utc::now() > deadline {
        let reason = if exec.poll_count > max_polls {
            "max_polls"
        } else {
            "max_wait_ms"
        };
        finish_timeout(ctx, db, exec, &poll_url, reason).await;
        return;
    }

    // Load endpoint + resolve headers
    let endpoint = match db::endpoints::get(db, &exec.endpoint).await {
        Ok(Some(ep)) => ep,
        _ => {
            let _ = db::executions::complete_failed(db, execution_id).await;
            return;
        }
    };
    let async_cfg = match endpoint.get_async_config() {
        Some(c) => c,
        None => {
            let err = serde_json::json!({"type":"ENDPOINT_NO_LONGER_ASYNC"});
            let _ = db::executions::complete_failed_timeout(db, execution_id, &err).await;
            return;
        }
    };
    let Some(poll_cfg) = async_cfg.poll else {
        // callback-only endpoint — POLLING shouldn't happen; transition back to WAITING with a long sleep
        let next = std::cmp::min(deadline, Utc::now() + Duration::milliseconds(60_000));
        let _ = db::executions::transition_back_to_waiting(db, execution_id, next).await;
        return;
    };

    let secret_values = match secrets::load(
        db,
        &ctx.encryption_key,
        &endpoint.spec,
        Some(&ctx.secret_cache),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(execution_id, "Secret resolution failed for poll: {}", e);
            // Treat as transient
            let next = std::cmp::min(
                deadline,
                Utc::now() + Duration::milliseconds(poll_backoff(&poll_cfg, exec.poll_count)),
            );
            let _ = db::executions::transition_back_to_waiting(db, execution_id, next).await;
            return;
        }
    };

    // Build GET with resolved headers (only secret substitution; configs aren't relevant for polling URL)
    let mut req = ctx.http_client.get(&poll_url);
    if let Some(headers) = endpoint.spec.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                let resolved = substitute_secrets(s, &secret_values);
                req = req.header(k.as_str(), resolved);
            }
        }
    }
    let timeout_ms = endpoint
        .spec
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(5000);
    req = req.timeout(std::time::Duration::from_millis(timeout_ms));

    let started = std::time::Instant::now();
    let res = req.send().await;

    let polled_at = Utc::now();
    let duration_ms = started.elapsed().as_millis() as i64;
    let poll_number = exec.poll_count;

    match res {
        Ok(response) => {
            let status_code = response.status().as_u16();
            let retry_after_ms = parse_retry_after(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok()),
            );
            let body = response.text().await.unwrap_or_default();
            let parsed_body = serde_json::from_str::<serde_json::Value>(&body)
                .unwrap_or_else(|_| serde_json::json!({"raw": body}));

            let cls = classify(
                Some(status_code),
                &poll_cfg.success_statuses,
                &poll_cfg.pending_statuses,
                &poll_cfg.failure_statuses,
            );

            let _ = db::polls::insert(
                db,
                execution_id,
                poll_number,
                polled_at,
                Some(duration_ms),
                Some(status_code as i32),
                retry_after_ms,
                cls,
                None,
            )
            .await;

            metrics::counter!(m::POLLS_TOTAL, "classification" => cls.as_str().to_string())
                .increment(1);
            metrics::histogram!(invokr_common::metrics::POLL_DURATION_SECONDS)
                .record(duration_ms as f64 / 1000.0);

            match cls {
                PollClassification::SUCCESS => {
                    let _ = db::executions::complete_success_from_long_running(
                        db,
                        execution_id,
                        &parsed_body,
                    )
                    .await;
                    metrics::counter!(m::LONG_RUNNING_COMPLETED_TOTAL,
                        "terminator" => "poll",
                        "status" => "SUCCESS",
                    )
                    .increment(1);
                    metrics::gauge!(invokr_common::metrics::EXECUTIONS_WAITING).decrement(1.0);
                    log_execution(
                        db,
                        execution_id,
                        attempt_count,
                        "INFO",
                        &format!(
                            "Poll #{poll_number} → {status_code} success after {duration_ms}ms"
                        ),
                    )
                    .await;
                }
                PollClassification::TERMINAL_FAILURE => {
                    let retry_policy = endpoint.get_retry_policy();
                    let backoff_ms = backoff::compute_backoff(&retry_policy, attempt_count);
                    let _ = db::executions::retry_from_poll(db, execution_id, backoff_ms).await;
                    metrics::counter!(m::LONG_RUNNING_COMPLETED_TOTAL,
                        "terminator" => "poll",
                        "status" => "FAILED",
                    )
                    .increment(1);
                    metrics::gauge!(invokr_common::metrics::EXECUTIONS_WAITING).decrement(1.0);
                    log_execution(db, execution_id, attempt_count, "WARN",
                        &format!("Poll #{poll_number} → {status_code} terminal failure; re-dispatch in {backoff_ms}ms")).await;
                }
                PollClassification::TRANSIENT_ERROR
                    if transient_cap_reached(db, &poll_cfg, execution_id).await =>
                {
                    redispatch(
                        db,
                        &endpoint,
                        execution_id,
                        attempt_count,
                        &format!("Poll #{poll_number} → {status_code}"),
                    )
                    .await;
                }
                PollClassification::PENDING | PollClassification::TRANSIENT_ERROR => {
                    // Retry-After from the destination wins; otherwise back off
                    // per the endpoint's poll spec.
                    let delay_ms =
                        retry_after_ms.unwrap_or_else(|| poll_backoff(&poll_cfg, poll_number));
                    let next =
                        std::cmp::min(deadline, Utc::now() + Duration::milliseconds(delay_ms));
                    let _ =
                        db::executions::transition_back_to_waiting(db, execution_id, next).await;
                    log_execution(
                        db,
                        execution_id,
                        attempt_count,
                        "INFO",
                        &format!(
                            "Poll #{poll_number} → {status_code} ({}); next poll in {}ms",
                            cls.as_str(),
                            delay_ms
                        ),
                    )
                    .await;
                }
            }
        }
        Err(e) => {
            let err = serde_json::json!({"type":"TRANSPORT_ERROR","message":e.to_string()});
            let _ = db::polls::insert(
                db,
                execution_id,
                poll_number,
                polled_at,
                Some(duration_ms),
                None,
                None,
                PollClassification::TRANSIENT_ERROR,
                Some(&err),
            )
            .await;
            metrics::counter!(m::POLLS_TOTAL, "classification" => "TRANSIENT_ERROR").increment(1);
            metrics::histogram!(invokr_common::metrics::POLL_DURATION_SECONDS)
                .record(duration_ms as f64 / 1000.0);
            if transient_cap_reached(db, &poll_cfg, execution_id).await {
                redispatch(
                    db,
                    &endpoint,
                    execution_id,
                    attempt_count,
                    &format!("Poll #{poll_number} transport error"),
                )
                .await;
                return;
            }
            let delay_ms = poll_backoff(&poll_cfg, poll_number);
            let next = std::cmp::min(deadline, Utc::now() + Duration::milliseconds(delay_ms));
            let _ = db::executions::transition_back_to_waiting(db, execution_id, next).await;
            log_execution(
                db,
                execution_id,
                attempt_count,
                "WARN",
                &format!("Poll #{poll_number} transport error; next poll in {delay_ms}ms"),
            )
            .await;
        }
    }
}

/// End a poll cycle whose wait or poll budget ran out. Re-dispatches while
/// attempts remain; on the last one tells the destination to stop and fails.
///
/// The DELETE is deliberately skipped when a retry follows: the re-dispatch
/// carries the same idempotency key, so the destination can resume the work
/// this would otherwise have thrown away.
async fn finish_timeout(
    ctx: &PipelineContext,
    db: &mut DbContext<'_>,
    exec: &invokr_common::db::executions::ClaimedExecution,
    poll_url: &str,
    reason: &str,
) {
    let execution_id = &exec.execution_id;
    let endpoint = db::endpoints::get(db, &exec.endpoint).await.ok().flatten();
    let last_attempt = exec.attempt_count >= exec.max_attempts;

    if last_attempt {
        if let Some(ep) = &endpoint {
            send_stop_delete(ctx, db, ep, poll_url, execution_id).await;
        }
    }

    let backoff_ms = endpoint
        .as_ref()
        .map(|ep| backoff::compute_backoff(&ep.get_retry_policy(), exec.attempt_count))
        .unwrap_or(0);
    let err = serde_json::json!({"type": "TIMEOUT", "reason": reason});
    let _ = db::executions::retry_from_long_running(db, execution_id, backoff_ms, &err).await;

    let msg = if last_attempt {
        format!("Polling budget exhausted ({reason}); no attempts left, marking FAILED")
    } else {
        format!("Polling budget exhausted ({reason}); re-dispatching in {backoff_ms}ms")
    };
    log_execution(db, execution_id, exec.attempt_count, "WARN", &msg).await;
}

/// Best-effort "stop working" to the poll URL. The destination may not implement
/// DELETE, so the outcome is only recorded on the execution.
async fn send_stop_delete(
    ctx: &PipelineContext,
    db: &mut DbContext<'_>,
    endpoint: &invokr_common::models::Endpoint,
    poll_url: &str,
    execution_id: &str,
) {
    let secret_values = secrets::load(
        db,
        &ctx.encryption_key,
        &endpoint.spec,
        Some(&ctx.secret_cache),
    )
    .await
    .unwrap_or_default();
    let mut req = ctx
        .http_client
        .delete(poll_url)
        .timeout(std::time::Duration::from_secs(5));
    if let Some(headers) = endpoint.spec.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), substitute_secrets(s, &secret_values));
            }
        }
    }
    let line = match req.send().await {
        Ok(r) => format!("Timeout DELETE to {poll_url} → {}", r.status().as_u16()),
        Err(e) => format!("Timeout DELETE to {poll_url} → error: {e}"),
    };
    log_execution(db, execution_id, 0, "INFO", &line).await;
}

/// True once `max_consecutive_errors` transient polls have landed back to back.
async fn transient_cap_reached(
    db: &mut DbContext<'_>,
    poll_cfg: &PollConfig,
    execution_id: &str,
) -> bool {
    let Some(cap) = poll_cfg.max_consecutive_errors else {
        return false;
    };
    db::polls::consecutive_transient_errors(db, execution_id)
        .await
        .unwrap_or(0)
        >= cap as i64
}

/// Treat the current attempt as failed and hand it back to the retry path,
/// which re-dispatches or fails depending on `max_attempts`.
async fn redispatch(
    db: &mut DbContext<'_>,
    endpoint: &invokr_common::models::Endpoint,
    execution_id: &str,
    attempt_count: i64,
    context: &str,
) {
    let backoff_ms = backoff::compute_backoff(&endpoint.get_retry_policy(), attempt_count);
    let _ = db::executions::retry_from_poll(db, execution_id, backoff_ms).await;
    log_execution(
        db,
        execution_id,
        attempt_count,
        "WARN",
        &format!("{context}; too many consecutive errors, re-dispatch in {backoff_ms}ms"),
    )
    .await;
}

/// Delay before the next poll, from the endpoint's `async.poll` spec.
///
/// `poll_number` is 1-based (the claim increments `poll_count` before handing the
/// execution over), so the first wait is exactly `initial_delay_ms` and it grows
/// from there, capped at `max_delay_ms`. Callers apply `Retry-After` in
/// preference to this when the destination sent one.
fn poll_backoff(poll_cfg: &PollConfig, poll_number: i32) -> i64 {
    invokr_common::backoff::compute_backoff_ms(
        &poll_cfg.backoff,
        poll_cfg.initial_delay_ms,
        poll_cfg.max_delay_ms,
        poll_number as i64,
    )
}

fn substitute_secrets(s: &str, secrets: &HashMap<String, String>) -> String {
    let mut out = s.to_string();
    for (k, v) in secrets {
        out = out.replace(&format!("{{{{secret.{k}}}}}"), v);
    }
    out
}

async fn log_execution(
    db: &mut DbContext<'_>,
    execution_id: &str,
    attempt_number: i64,
    level: &str,
    message: &str,
) {
    let _ =
        invokr_common::db::execution_logs::insert(db, execution_id, attempt_number, level, message)
            .await;
}
