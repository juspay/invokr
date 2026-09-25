use metrics_exporter_prometheus::PrometheusHandle;

// --- Counters ---
pub const JOBS_CREATED_TOTAL: &str = "invokr_jobs_created_total";
pub const EXECUTIONS_CLAIMED_TOTAL: &str = "invokr_executions_claimed_total";
pub const EXECUTIONS_COMPLETED_TOTAL: &str = "invokr_executions_completed_total";
pub const WORKER_POLL_IDLE_TOTAL: &str = "invokr_worker_poll_idle_total";
pub const CRON_JOBS_REAPED_TOTAL: &str = "invokr_cron_jobs_reaped_total";

// Dispatcher-level counters
pub const DISPATCH_TOTAL: &str = "invokr_dispatch_total";
pub const KAFKA_MESSAGES_PRODUCED_TOTAL: &str = "invokr_kafka_messages_produced_total";
pub const REDIS_STREAM_MESSAGES_SENT_TOTAL: &str = "invokr_redis_stream_messages_sent_total";

// --- Histograms ---
pub const EXECUTION_DURATION_SECONDS: &str = "invokr_execution_duration_seconds";
pub const DISPATCH_DURATION_SECONDS: &str = "invokr_dispatch_duration_seconds";

// --- Gauges ---
pub const WORKER_INFLIGHT: &str = "invokr_worker_inflight_executions";

/// Install the Prometheus recorder and return a handle for rendering metrics.
pub fn install_recorder() -> PrometheusHandle {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}
