# invokr

![Version: 0.1.0](https://img.shields.io/badge/Version-0.1.0-informational?style=flat-square) ![Type: application](https://img.shields.io/badge/Type-application-informational?style=flat-square) ![AppVersion: 0.1.0](https://img.shields.io/badge/AppVersion-0.1.0-informational?style=flat-square)

A Helm chart for Invokr — a multi-tenant job scheduling and delivery service

Invokr runs as **two workloads** sharing one PostgreSQL database:

| Workload | Port | Purpose |
|---|---|---|
| `api` | 8080 | REST API, and the dashboard when `apiConfigs.mode` is `both` |
| `worker` | 9090 | Polls the database and delivers jobs. Metrics only — no inbound traffic |

## Requirements before installing

**The database must have `pg_cron` enabled at the server level.** Invokr does not
schedule CRON jobs itself — it delegates to the extension. Without it everything
installs cleanly and CRON jobs silently never fire.

1. `shared_preload_libraries = pg_cron` — a *static* parameter, so it needs a
   database restart before it takes effect
2. `cron.database_name = invokr_db`
3. `CREATE EXTENSION pg_cron` — requires superuser (`rds_superuser` on RDS/Aurora)

Point `secrets.database_url` at the **writer** endpoint. pg_cron runs jobs only
on the writer.

## Installing

Four values must be set; everything else has a working default. The image tag
defaults to the chart's `appVersion`, which the release workflow keeps equal to
the released version, so it only needs setting to pin a different build.

```bash
helm install invokr ./helm \
  --namespace invokr --create-namespace \
  --set secrets.database_url='postgresql://user:pass@writer-host:5432/invokr_db' \
  --set secrets.api_key='<api key>' \
  --set secrets.encryption_key='<64 hex chars>' \
  --set database.host=writer-host
```

Released charts are published alongside the images:

```bash
helm install invokr oci://ghcr.io/juspay/helm-charts/invokr --version <version>
```

The chart refuses to render if a required value is missing, naming which one —
so there is nothing to memorise.

## Migrations

Migrations are compiled into the api image, so the chart runs that same image as
a `pre-install,pre-upgrade` hook Job. Helm blocks on hook Jobs, so a failed
migration aborts the release **before any pod is replaced** — the failure mode is
"nothing changed", not "half changed".

Set `migration.mode: dry-run` to have the Job print the SQL it would apply
instead of applying it, or `migration.enabled: false` to hand migrations to a DBA
entirely. App pods never migrate under any setting.

In clusters running external-secrets, leave `secrets` empty and point
`existingSecret` at the Secret your ExternalSecret produces. It must contain
`INVOKR_DATABASE_URL`, `INVOKR_API_KEY` and `INVOKR_ENCRYPTION_KEY`.

## Configuration model

Every key under `configs`, `apiConfigs` and `workerConfigs` is rendered into a
ConfigMap as `INVOKR_<KEY_UPPERCASED>` and injected via `envFrom`. **Adding a new
setting is a one-line change in `values.yaml`** — no template edit. Set a key to
`null` to omit it and fall back to the application default.

`configs.path_prefix` is the single source of truth for the URL prefix: it feeds
the probes, the ingress paths, the ServiceMonitor's metrics path and the
dashboard's config. Change it in one place.

## Pulling from a mirror

`global.imageRegistry` overrides the registry for every image, so mirroring into
ECR needs one value rather than a chart fork:

```yaml
global:
  imageRegistry: <account>.dkr.ecr.<region>.amazonaws.com
```

## Before exposing the dashboard

The dashboard is served by the API pods and renders `INVOKR_API_KEY` into the
page. That key is not scoped — it authorises every workspace on the install — so
anyone who can load the dashboard has full API access. Keep it off the public
internet until per-user authentication ships.

## Values

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| affinity | object | `{}` | Affinity for all workloads. Overrides `global.affinity`. |
| api.autoscaling.enabled | bool | `false` | Enable an HPA for the API. |
| api.autoscaling.maxReplicas | int | `6` |  |
| api.autoscaling.minReplicas | int | `2` |  |
| api.autoscaling.targetCPUUtilizationPercentage | int | `80` |  |
| api.ingress.annotations | object | `{}` |  |
| api.ingress.className | string | `""` |  |
| api.ingress.enabled | bool | `false` | Expose the API through an Ingress. Leave disabled when using Istio. The dashboard is served by this same Service. |
| api.ingress.hosts[0].host | string | `"invokr.local"` |  |
| api.ingress.hosts[0].paths[0].path | string | `"/invokr"` |  |
| api.ingress.hosts[0].paths[0].pathType | string | `"Prefix"` |  |
| api.ingress.tls | list | `[]` |  |
| api.livenessProbe | object | `{"failureThreshold":3,"initialDelaySeconds":20,"periodSeconds":10,"timeoutSeconds":5}` | Liveness probe. The path prefix is prepended automatically. |
| api.podAnnotations | object | `{}` | Extra pod annotations. |
| api.podDisruptionBudget | object | `{"enabled":false,"minAvailable":1}` | PodDisruptionBudget. Keep `minAvailable` below `replicaCount`, or nodes become undrainable. |
| api.podLabels | object | `{}` | Extra pod labels. |
| api.readinessProbe | object | `{"failureThreshold":3,"initialDelaySeconds":5,"periodSeconds":5,"timeoutSeconds":3}` | Readiness probe. |
| api.replicaCount | int | `2` | Replicas when autoscaling is disabled. |
| api.repository | string | `"invokr-api"` | Image repository for the API server. |
| api.resources | object | `{}` | Resource requests and limits. |
| api.service.port | int | `80` | Port the Service exposes. |
| api.service.targetPort | int | `8080` | Container port the API listens on. |
| api.service.type | string | `"ClusterIP"` | Service type. |
| api.terminationGracePeriodSeconds | int | `30` | Grace period for in-flight HTTP requests on shutdown. |
| apiConfigs | object | `{"mode":"both"}` | Settings for the api workload. INVOKR_LISTEN_ADDR is derived from `api.service.targetPort`, not set here. |
| apiConfigs.mode | string | `"both"` | `api`, `dashboard` or `both`. |
| configs | object | `{"db_pool_size":20,"kms_enabled":false,"path_prefix":"/invokr"}` | Non-secret settings shared by both workloads. |
| configs.db_pool_size | int | `20` | Connection pool size, PER POD. Multiply by total replicas and compare against the database's max_connections before scaling. |
| configs.kms_enabled | bool | `false` | Whether secrets arrive as base64 KMS ciphertext. Requires `kms.enabled`. |
| configs.path_prefix | string | `"/invokr"` | URL prefix the API is served under. Single source of truth: feeds the ingress path, probes, ServiceMonitor path and dashboard config. |
| dashboard.enabled | bool | `true` | Serve the web dashboard. Requires `apiConfigs.mode` to be `both` or `dashboard`. |
| dashboard.pathPrefix | string | `"/dashboard"` | URL prefix the dashboard is served under. |
| database.host | string | `""` | Database host. Required when `migration.enabled` — the Job's pg_isready check needs it, and it cannot be parsed out of the connection string. |
| database.name | string | `"invokr_db"` | Database name. |
| database.port | int | `5432` | Database port. |
| database.user | string | `"invokr"` | Database user. |
| existingSecret | string | `""` | Use an existing Secret instead of rendering one. Must contain INVOKR_DATABASE_URL, INVOKR_API_KEY and INVOKR_ENCRYPTION_KEY. |
| fullnameOverride | string | `""` | Override the generated fullname. |
| global | object | `{"affinity":{},"imageRegistry":null,"nodeSelector":{},"tolerations":[]}` | Global values, shared with any parent chart. |
| global.affinity | object | `{}` | Affinity applied to every workload unless overridden. |
| global.imageRegistry | string | `nil` | Overrides `image.registry` for every image. Set to an ECR host to pull from a mirror. |
| global.nodeSelector | object | `{}` | Node selector applied to every workload unless overridden. |
| global.tolerations | list | `[]` | Tolerations applied to every workload unless overridden. |
| image.pullPolicy | string | `"IfNotPresent"` | Image pull policy. |
| image.registry | string | `"ghcr.io/juspay"` | Registry hosting the Invokr images. |
| image.tag | string | `""` | Image tag. Defaults to `.Chart.AppVersion`, which the release workflow keeps equal to the released semver. Set it only to pin a different build. |
| imagePullSecrets | list | `[]` | Secrets for pulling from a private registry. |
| istio.destinationRule.enabled | bool | `false` | Create a DestinationRule for the API service. |
| istio.destinationRule.trafficPolicy | object | `{}` | Traffic policy. |
| istio.enabled | bool | `false` | Enable Istio resources. |
| istio.virtualService.enabled | bool | `false` | Create a VirtualService routing to the API. |
| istio.virtualService.gateways | list | `[]` | Gateways the VirtualService attaches to. |
| istio.virtualService.hosts | list | `[]` | Hosts the VirtualService matches. |
| istio.virtualService.http | list | `[]` | Routing rules. The destination is set to the API service automatically. |
| kms | object | `{"enabled":false}` | Selects the `-kms` image variants, which expect `secrets` to hold base64 KMS ciphertext. Grant decrypt permission via `serviceAccount.annotations`. |
| migration.args | list | `["migrate"]` | Arguments passed to the api image to run migrations and exit. |
| migration.enabled | bool | `true` | Run migrations as a pre-install/pre-upgrade hook. Helm blocks on hook Jobs, so a failed migration aborts the release before any pod is replaced. |
| migration.mode | string | `"run"` | INVOKR_DB_MIGRATION_MODE for the Job. `run` applies pending migrations; `dry-run` prints the SQL without applying it, for review or for baselining a database whose schema was applied by hand. App pods never migrate. |
| migration.resources | object | `{}` | Resources for the migration Job. |
| migration.waitForDb | object | `{"image":"postgres:16-alpine","maxAttempts":30,"registry":"docker.io","sleepSeconds":5}` | Init container that waits for PostgreSQL. |
| nameOverride | string | `""` | Override the chart name. |
| nodeSelector | object | `{}` | Node selector for all workloads. Overrides `global.nodeSelector`. |
| podSecurityContext | object | `{}` | Pod-level security context. Empty because the published images do not declare a non-root USER, so `runAsNonRoot` would stop every pod starting. |
| secrets.api_key | string | `""` | Bearer token for the REST API. Also served to the dashboard in the browser, so anyone who can load the dashboard holds full API access. |
| secrets.database_url | string | `""` |  |
| secrets.encryption_key | string | `""` | 32-byte hex key encrypting stored secrets at rest. |
| securityContext | object | `{}` | Container-level security context. |
| serviceAccount.annotations | object | `{}` | Annotations. Add the IRSA role ARN here when `kms.enabled`. |
| serviceAccount.automount | bool | `false` | Automount the ServiceAccount's API credentials. |
| serviceAccount.create | bool | `true` | Create a ServiceAccount. |
| serviceAccount.name | string | `""` | Name. Generated from the fullname when empty. |
| serviceMonitor.enabled | bool | `false` | Create ServiceMonitors for the Prometheus Operator. |
| serviceMonitor.interval | string | `"30s"` | Scrape interval. |
| serviceMonitor.labels | object | `{}` | Labels matching the operator's serviceMonitorSelector, commonly `release: kube-prometheus-stack`. The wrong label scrapes nothing, silently. |
| serviceMonitor.scrapeTimeout | string | `"10s"` | Scrape timeout. |
| tolerations | list | `[]` | Tolerations for all workloads. Overrides `global.tolerations`. |
| worker.autoscaling.enabled | bool | `false` | Enable an HPA for the worker. CPU is a poor proxy for queue depth: a worker blocked on slow deliveries looks idle. |
| worker.autoscaling.maxReplicas | int | `10` |  |
| worker.autoscaling.minReplicas | int | `2` |  |
| worker.autoscaling.targetCPUUtilizationPercentage | int | `80` |  |
| worker.livenessProbe | object | `{"failureThreshold":3,"initialDelaySeconds":20,"periodSeconds":15,"timeoutSeconds":5}` | Liveness probe against `/health`, which checks no dependencies. |
| worker.podAnnotations | object | `{}` | Extra pod annotations. |
| worker.podDisruptionBudget | object | `{"enabled":false,"minAvailable":1}` | PodDisruptionBudget. Keep `minAvailable` below `replicaCount`, or nodes become undrainable. |
| worker.podLabels | object | `{}` | Extra pod labels. |
| worker.readinessProbe | object | `{"failureThreshold":3,"initialDelaySeconds":10,"periodSeconds":10,"timeoutSeconds":5}` | Readiness probe against `/ready`, which checks the database and poll-loop staleness. A saturated worker still reports ready. |
| worker.replicaCount | int | `2` | Replicas when autoscaling is disabled. Executions are claimed transactionally, so running several is safe. |
| worker.repository | string | `"invokr-worker"` | Image repository for the worker. |
| worker.resources | object | `{}` | Resource requests and limits. |
| worker.terminationGracePeriodSeconds | int | `45` | MUST exceed `workerConfigs.worker_shutdown_timeout_sec`, or SIGKILL cuts the drain short. |
| workerConfigs | object | `{"config_cache_ttl_sec":60,"cron_batch_size":100,"cron_tick_interval_sec":1,"health_db_probe_timeout_ms":2000,"health_server_workers":1,"health_stale_after_floor_ms":5000,"metrics_port":9090,"promote_interval_ms":500,"reaper_cron_expression":"*/15 * * * *","reclaim_interval_sec":30,"secret_cache_ttl_sec":300,"stuck_execution_timeout_sec":300,"worker_max_concurrent":50,"worker_poll_interval_ms":200,"worker_shutdown_timeout_sec":30}` | Settings for the worker workload. |
| workerConfigs.config_cache_ttl_sec | int | `60` | Config cache TTL, in seconds. |
| workerConfigs.cron_batch_size | int | `100` | Maximum CRON jobs materialised per tick. |
| workerConfigs.cron_tick_interval_sec | int | `1` | CRON tick interval, in seconds. |
| workerConfigs.health_db_probe_timeout_ms | int | `2000` | Timeout for `/ready`'s SELECT 1, in milliseconds. |
| workerConfigs.health_server_workers | int | `1` | Worker threads for the ops server. |
| workerConfigs.health_stale_after_floor_ms | int | `5000` | Floor on the poll-loop staleness threshold, in milliseconds. Effective threshold is max(this, 10 x worker_poll_interval_ms). |
| workerConfigs.metrics_port | int | `9090` | Worker ops server port. Serves /health, /ready and /metrics, unprefixed. |
| workerConfigs.promote_interval_ms | int | `500` | Promotion sweep interval, in milliseconds. |
| workerConfigs.reaper_cron_expression | string | `"*/15 * * * *"` | pg_cron expression for the sweep that retires expired CRON jobs. |
| workerConfigs.reclaim_interval_sec | int | `30` | Reclaim sweep interval for stuck executions, in seconds. |
| workerConfigs.secret_cache_ttl_sec | int | `300` | Secret cache TTL, in seconds. |
| workerConfigs.stuck_execution_timeout_sec | int | `300` | Age at which a RUNNING execution is reclaimed, in seconds. |
| workerConfigs.worker_max_concurrent | int | `50` | Maximum executions a single worker processes concurrently. |
| workerConfigs.worker_poll_interval_ms | int | `200` | Poller sleep after finding no work, in milliseconds. |
| workerConfigs.worker_shutdown_timeout_sec | int | `30` | Drain grace period for in-flight executions. `worker.terminationGracePeriodSeconds` must exceed this. |

----------------------------------------------
Autogenerated from chart metadata using [helm-docs v1.14.2](https://github.com/norwoodj/helm-docs/releases/v1.14.2)
