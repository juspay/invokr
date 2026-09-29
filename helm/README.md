# invokr

![Version: 0.3.1](https://img.shields.io/badge/Version-0.3.1-informational?style=flat-square) ![Type: application](https://img.shields.io/badge/Type-application-informational?style=flat-square) ![AppVersion: 0.3.1](https://img.shields.io/badge/AppVersion-0.3.1-informational?style=flat-square)

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

Two values must be set; everything else has a working default. The image tag
defaults to the chart's `appVersion`, which the release workflow keeps equal to
the released version, so it only needs setting to pin a different build.

```bash
helm install invokr ./helm \
  --namespace invokr --create-namespace \
  --set secrets.database_url='postgresql://user:pass@writer-host:5432/invokr_db' \
  --set secrets.encryption_key='<64 hex chars>'
```

The chart ships no `secrets.api_key`. `INVOKR_API_KEY` is the pre-OIDC shared
key, kept in the API only so existing installs can migrate off it; under `oidc`
it installs an extra authenticator that bypasses per-user identity, and the API
warns at every startup while it is set. A new deployment has nothing to migrate,
so the chart does not offer it. `secrets` is a free-form map, so an install that
genuinely still needs it can set `secrets.api_key` and it will render.

`database.host` becomes required too if you enable `migration.enabled` — the
Job's `pg_isready` check needs a host, and it cannot be parsed out of the
connection string.

Released charts are published alongside the images:

```bash
helm install invokr oci://ghcr.io/juspay/helm-charts/invokr --version <version>
```

The chart refuses to render if a required value is missing, naming which one —
so there is nothing to memorise.

## Migrations

**`migration.enabled` is `false`, and schema migrations are a manual step today.**
The api image has no migrate-and-exit path, so the Job would start an API server
that never exits and the pre-install hook would hang until Helm's timeout. Apply
the SQL from the repository's `migrations/` directory in filename order before
serving traffic.

The Job itself is complete and ships disabled. Once the image supports
`migrate`, set `migration.enabled: true` and the chart runs it as a
`pre-install,pre-upgrade` hook — Helm blocks on hook Jobs, so a failed migration
aborts the release **before any pod is replaced**, making the failure mode
"nothing changed" rather than "half changed". `migration.mode: dry-run` will then
print the SQL instead of applying it. App pods never migrate under any setting.

In clusters running external-secrets, leave `secrets` empty and point
`existingSecret` at the Secret your ExternalSecret produces. It must contain
`INVOKR_DATABASE_URL` and `INVOKR_ENCRYPTION_KEY`.

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

## Before exposing the API

`apiConfigs.auth_mode` decides whether anything is authenticated, and it defaults
to `disabled` — every request is served as a development identity, with no
credential required or checked. Anyone who can reach the pods has full access to
every workspace, which is why `api.ingress.enabled` defaults to `false`.

Set `auth_mode: oidc` and register a client with your IdP before exposing it:

```yaml
apiConfigs:
  auth_mode: oidc
  oidc_issuer_url: https://your-idp/
  oidc_client_id: invokr
  oidc_redirect_host: https://invokr.your-domain
secrets:
  oidc_client_secret: <secret>
```

The dashboard is served by the same pods and authenticates with a same-origin
session cookie, so it is covered by the same setting — it no longer carries a
service-wide key.

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
| api.ingress.hosts | list | `[{"host":"invokr.local"}]` | Hosts to route. Omit `paths` (as below) to derive them from `configs.path_prefix` and, when the dashboard is enabled, `dashboard.pathPrefix` -- the dashboard is a sibling of the API prefix, not a child, so routing only the API prefix leaves it 404ing. Set `paths` explicitly to override, e.g. `[{path: /invokr, pathType: Prefix}]`. |
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
| apiConfigs | object | `{"auth_mode":"disabled","mode":"both"}` | Settings for the api workload. INVOKR_LISTEN_ADDR is derived from `api.service.targetPort`, not set here. |
| apiConfigs.auth_mode | string | `"disabled"` | `disabled` or `oidc`. Has no default in the API, which refuses to start without it rather than pick an auth posture for itself; the chart supplies one so a default install boots. `disabled` authenticates every request as a development identity -- combine it with `api.ingress.enabled: false`. For `oidc`, add `oidc_issuer_url`, `oidc_client_id` and `oidc_redirect_host` here, and `oidc_client_secret` under `secrets`; any key in these maps is rendered as `INVOKR_<KEY>`. |
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
| existingSecret | string | `""` | Use an existing Secret instead of rendering one. Must contain INVOKR_DATABASE_URL and INVOKR_ENCRYPTION_KEY. |
| extraEnv | list | `[]` | Extra environment variables, in Kubernetes `env` form. |
| extraEnvFrom | list | `[]` | Extra `envFrom` sources. Rendered after the chart's own ConfigMaps and Secret, so a key set here wins on collision. |
| fullnameOverride | string | `""` | Override the generated fullname. |
| global | object | `{"affinity":{},"imageRegistry":null,"nodeSelector":{},"tolerations":[]}` | Global values, shared with any parent chart. |
| global.affinity | object | `{}` | Affinity applied to every workload unless overridden. |
| global.imageRegistry | string | `nil` | Overrides `image.registry` for every image. Set to an ECR host to pull from a mirror. |
| global.nodeSelector | object | `{}` | Node selector applied to every workload unless overridden. |
| global.tolerations | list | `[]` | Tolerations applied to every workload unless overridden. |
| image.pullPolicy | string | `"IfNotPresent"` | Image pull policy. |
| image.registry | string | `"ghcr.io/juspay/invokr"` | Registry and path hosting the Invokr images, everything before the image name. The release workflow publishes under `ghcr.io/<owner>/invokr`, so the `/invokr` segment is part of this value, not of `repository`. A mirror must keep that shape: `<account>.dkr.ecr.<region>.amazonaws.com/juspay/invokr`. |
| image.tag | string | `""` | Image tag. Defaults to `.Chart.AppVersion`, which the release workflow keeps equal to the released semver. Set it only to pin a different build. |
| imagePullSecrets | list | `[]` | Secrets for pulling from a private registry. |
| istio.destinationRule.enabled | bool | `false` | Create a DestinationRule for the API service. |
| istio.destinationRule.trafficPolicy | object | `{}` | Traffic policy. |
| istio.enabled | bool | `false` | Enable Istio resources. |
| istio.virtualService.enabled | bool | `false` | Create a VirtualService routing to the API. |
| istio.virtualService.gateways | list | `[]` | Gateways the VirtualService attaches to. |
| istio.virtualService.hosts | list | `[]` | Hosts the VirtualService matches. |
| istio.virtualService.http | list | `[]` | Routing rules. The destination is set to the API service automatically. |
| kms | object | `{"enabled":false}` | Selects the `-kms` image variants, which expect `secrets` to hold base64 KMS ciphertext. Grant decrypt permission via `serviceAccount.annotations`, and set AWS_REGION via `extraEnv` -- the SDK defaults to us-east-1, not to the cluster's region. |
| migration.args | list | `["migrate"]` | Arguments passed to the api image to run migrations and exit. |
| migration.enabled | bool | `false` | Run migrations as a pre-install/pre-upgrade hook. Helm blocks on hook Jobs, so a failed migration aborts the release before any pod is replaced.  OFF by default: the api image has no migrate-and-exit path yet, so the Job would start an API server that never exits and the hook would hang until Helm's timeout. Apply migrations/*.sql by hand until that lands, then set this to true. |
| migration.mode | string | `"run"` | INVOKR_DB_MIGRATION_MODE for the Job. `run` applies pending migrations; `dry-run` prints the SQL without applying it, for review or for baselining a database whose schema was applied by hand. App pods never migrate. |
| migration.resources | object | `{}` | Resources for the migration Job. |
| migration.waitForDb | object | `{"image":"postgres:16-alpine","maxAttempts":30,"registry":"docker.io","sleepSeconds":5}` | Init container that waits for PostgreSQL. |
| nameOverride | string | `""` | Override the chart name. |
| nodeSelector | object | `{}` | Node selector for all workloads. Overrides `global.nodeSelector`. |
| podSecurityContext | object | `{}` | Pod-level security context. Empty because the published images do not declare a non-root USER, so `runAsNonRoot` would stop every pod starting. |
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
| workerConfigs | object | `{"config_cache_ttl_sec":60,"metrics_port":9090,"reaper_cron_expression":"*/15 * * * *","secret_cache_ttl_sec":300,"worker_max_concurrent":50,"worker_poll_interval_ms":200,"worker_shutdown_timeout_sec":30}` | Settings for the worker workload. |
| workerConfigs.config_cache_ttl_sec | int | `60` | Config cache TTL, in seconds. |
| workerConfigs.metrics_port | int | `9090` | Worker ops server port. Serves /health, /ready and /metrics, unprefixed. |
| workerConfigs.reaper_cron_expression | string | `"*/15 * * * *"` | pg_cron expression for the sweep that retires expired CRON jobs. |
| workerConfigs.secret_cache_ttl_sec | int | `300` | Secret cache TTL, in seconds. |
| workerConfigs.worker_max_concurrent | int | `50` | Maximum executions a single worker processes concurrently. |
| workerConfigs.worker_poll_interval_ms | int | `200` | Poller sleep after finding no work, in milliseconds. |
| workerConfigs.worker_shutdown_timeout_sec | int | `30` | Drain grace period for in-flight executions. `worker.terminationGracePeriodSeconds` must exceed this. |

----------------------------------------------
Autogenerated from chart metadata using [helm-docs v1.14.2](https://github.com/norwoodj/helm-docs/releases/v1.14.2)
