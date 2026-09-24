---
id: monitoring
title: Monitoring & Observability
---

# Monitoring & Observability

Invokr exposes Prometheus metrics from both the API server and the worker. A pre-built Grafana dashboard visualizes job creation, execution throughput, dispatch latency, and worker health.

## Metrics endpoints

| Component | Metrics endpoint | Default port |
|-----------|-----------------|:-----------:|
| API server | `GET /metrics` | 8080 (same as API) |
| Worker | `GET /metrics` (separate HTTP listener) | 9090 |
| Scheduler | `GET /metrics` (separate HTTP listener) | 9091 |

:::note
The API server serves metrics on the same port as the API (at `/metrics`). The worker and scheduler expose metrics on separate HTTP listeners, configured via `INVOKR_METRICS_PORT`.
:::

## Starting the monitoring stack

Invokr includes Docker Compose configurations for Prometheus and Grafana:

```bash
# Start Prometheus + Grafana
just monitoring-up
```

This starts:

| Service | URL | Credentials |
|---------|-----|------------|
| Prometheus | http://localhost:9099 | — |
| Grafana | http://localhost:3001 | admin / invokr |

:::info
Prometheus runs on host port **9099** (mapped to container port 9090) to avoid conflicts with the worker's metrics port.
:::

To stop the monitoring stack:

```bash
just monitoring-down
```

To start everything (infrastructure + monitoring):

```bash
just all-up
```

## Prometheus configuration

The Prometheus scrape config is at `monitoring/prometheus.yml`:

```yaml
global:
  scrape_interval: 5s
  evaluation_interval: 5s

scrape_configs:
  - job_name: "invokr-api"
    metrics_path: /metrics
    static_configs:
      - targets: ["host.docker.internal:8080"]
        labels:
          service: api

  - job_name: "invokr-worker"
    metrics_path: /metrics
    static_configs:
      - targets: ["host.docker.internal:9090"]
        labels:
          service: worker

  - job_name: "invokr-scheduler"
    metrics_path: /metrics
    static_configs:
      - targets: ["host.docker.internal:9091"]
        labels:
          service: scheduler
```

Prometheus scrapes all three components every 5 seconds. The `host.docker.internal` alias allows the Prometheus container to reach services running on the host.

## Key metrics

### Job & execution metrics

| Metric | Type | Description |
|--------|------|-------------|
| `invokr_jobs_created_total` | Counter | Jobs created, labeled by `trigger_type`, `endpoint`, `schema` |
| `invokr_executions_claimed_total` | Counter | Executions claimed by workers |
| `invokr_executions_completed_total` | Counter | Executions completed, labeled by `status` (`SUCCESS` / `FAILED`) |
| `invokr_execution_duration_seconds` | Histogram | End-to-end execution duration (claim to completion) |

### Dispatch metrics

| Metric | Type | Description |
|--------|------|-------------|
| `invokr_dispatch_total` | Counter | Dispatch attempts, labeled by `endpoint_type`, `status`, `error_type` |
| `invokr_dispatch_duration_seconds` | Histogram | Dispatcher-level latency (time to send and receive response) |

### Worker metrics

| Metric | Type | Description |
|--------|------|-------------|
| `invokr_worker_inflight_executions` | Gauge | Currently in-flight executions per worker |
| `invokr_worker_poll_idle_total` | Counter | Idle poll cycles (no work found) |

### Transport-specific metrics

| Metric | Type | Description |
|--------|------|-------------|
| `invokr_kafka_messages_produced_total` | Counter | Kafka messages produced, labeled by `topic`, `status` |
| `invokr_redis_stream_messages_sent_total` | Counter | Redis Stream messages sent, labeled by `stream`, `status` |

## Metric labels

The dispatch and completion metrics use consistent labels for filtering and grouping:

| Label | Values | Description |
|-------|--------|-------------|
| `endpoint_type` | `HTTP`, `KAFKA`, `REDIS_STREAM` | Transport type |
| `status` | `SUCCESS`, `FAILURE` | Dispatch outcome |
| `error_type` | `HTTP_ERROR`, `TIMEOUT`, `CONNECTION_ERROR`, `BROKER_ERROR`, `STREAM_ERROR` | Error classification (empty on success) |
| `trigger_type` | `IMMEDIATE`, `DELAYED`, `CRON` | Job trigger type |

## Pre-built Grafana dashboard

A pre-built Grafana dashboard is included at:

```
monitoring/grafana/dashboards/invokr-platform.json
```

Grafana is automatically provisioned with this dashboard on startup via the provisioning configuration at `monitoring/grafana/provisioning/`. The dashboard includes panels for:

- Jobs created over time (by trigger type)
- Execution throughput (claimed vs. completed)
- Execution duration histogram (p50, p90, p99)
- Dispatch success/failure rate by endpoint type
- Dispatch latency by endpoint type
- In-flight executions gauge
- Worker idle poll rate

:::tip
Access Grafana at http://localhost:3001 and log in with `admin` / `invokr`. The dashboard appears under **Dashboards → Invokr Platform**.
:::

## Useful PromQL queries

### Success rate by endpoint type

```promql
sum(rate(invokr_dispatch_total{status="SUCCESS"}[5m])) by (endpoint_type)
/
sum(rate(invokr_dispatch_total[5m])) by (endpoint_type)
```

### Execution duration p99

```promql
histogram_quantile(0.99, sum(rate(invokr_execution_duration_seconds_bucket[5m])) by (le))
```

### Dispatch error breakdown

```promql
sum(rate(invokr_dispatch_total{status="FAILURE"}[5m])) by (error_type)
```

### Worker utilization

```promql
invokr_worker_inflight_executions
```

### Idle poll rate (worker busyness indicator)

```promql
rate(invokr_worker_poll_idle_total[1m])
```

A high idle rate means the worker has spare capacity. A rate near zero means the worker is constantly finding work.

## Path prefix considerations

When running the API server with a path prefix (`INVOKR_PATH_PREFIX`), update the Prometheus scrape config to match:

```yaml
scrape_configs:
  - job_name: "invokr-api"
    metrics_path: /invokr/metrics    # was /metrics
    static_configs:
      - targets: ["host.docker.internal:8080"]
```

:::warning
If you use a path prefix and forget to update `metrics_path` in `prometheus.yml`, Prometheus will receive 404 errors and show no data for the API.
:::

Similarly, update Docker healthcheck URLs if using `docker-compose.prod.yml`:

```yaml
healthcheck:
  test: ["CMD", "curl", "-f", "http://localhost:8080/invokr/health"]
```

## Running with monitoring in development

For a complete local setup with monitoring:

```bash
# Start all infrastructure + monitoring
just all-up

# Start API + worker + mock-server
just dev
```

Then:

- **Prometheus**: http://localhost:9099 — view raw metrics, check scrape targets
- **Grafana**: http://localhost:3001 — view pre-built dashboard (admin / invokr)
- **API metrics**: http://localhost:8080/metrics — raw API Prometheus metrics
- **Worker metrics**: http://localhost:9090/metrics — raw worker Prometheus metrics

## Verifying metrics are flowing

Check that Prometheus is successfully scraping all targets:

```bash
# Check scrape target health
curl -s http://localhost:9099/api/v1/targets | python3 -m json.tool
```

All three targets (`invokr-api`, `invokr-worker`, `invokr-scheduler`) should show `"health": "up"`.

Query a metric directly:

```bash
# Total jobs created
curl -s "http://localhost:9099/api/v1/query?query=invokr_jobs_created_total" | python3 -m json.tool
```

## See also

- [Configuration](../configuration/environment-variables) — `INVOKR_METRICS_PORT` and other env vars
- [HTTP endpoints](./http-endpoints) — endpoint configuration
- [CRON jobs](./cron-jobs) — monitoring CRON job health via `/status`
- [Pagination](./pagination) — listing jobs and executions
