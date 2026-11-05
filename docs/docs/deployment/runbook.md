---
id: runbook
title: Fresh Deployment Runbook
---

# Fresh Deployment Runbook

This runbook covers a fresh production deployment of Invokr v0.5.0 on
Kubernetes with the Helm chart. A deployment is one PostgreSQL 16 database with
`pg_cron`, plus two stateless workloads: `invokr-api` and `invokr-worker`.
Schema migrations are applied by hand; neither image runs them.

| Item | Value |
| --- | --- |
| API image | `ghcr.io/juspay/invokr/invokr-api:0.5.0` (includes the dashboard) |
| Worker image | `ghcr.io/juspay/invokr/invokr-worker:0.5.0` |
| Helm chart | `oci://ghcr.io/juspay/helm-charts/invokr --version 0.5.0` |
| API port | 8080, under `INVOKR_PATH_PREFIX` (chart default `/invokr`) |
| Worker ops port | 9090: `/health`, `/ready`, `/metrics`, no prefix |

The published worker image is built without the `kafka` and `redis-stream`
features, so it delivers to HTTP endpoints only.

```text
Caller ──(Bearer ivk_<token>)──▶ invokr-api :8080 ──(OIDC)──▶ Identity provider
                                       │
                                       │ jobs, secrets, workspaces
                                       ▼
                     PostgreSQL 16 + pg_cron (writer endpoint)
                                       ▲
                                       │ polls executions
                                       │
       HTTP endpoints ◀── delivers ── invokr-worker :9090
```

When a KMS is selected, both pods also use it at startup to decrypt their
sensitive variables.

## Prerequisites

Have all of these ready before you install anything; most problems in a fresh
install come from a missing `pg_cron` setting.

- **PostgreSQL 16+** with the `pg_cron` package installed (the project's own
  image is `postgres:16` + `postgresql-16-cron`).
- **`shared_preload_libraries = pg_cron`**: this setting only takes effect after
  a database restart.
- **`cron.database_name = <invokr db name>`**: `pg_cron` runs jobs only in this
  one database.
- **A superuser** (`rds_superuser` on RDS/Aurora) to create the `pgcrypto` and
  `pg_cron` extensions.
- **The app's DB user** can create schemas in the database (each new workspace
  gets its own schema), and has `USAGE` on schema `cron` if it isn't a
  superuser.
- **The writer endpoint** for the connection string. `pg_cron` fires only on
  the writer.
- **Connection budget:** (API pods + worker pods) × `INVOKR_DB_POOL_SIZE` stays
  under `max_connections`. The code defaults the pool to 50 and the chart sets
  20.
- **An OIDC identity provider** (Keycloak, Okta, Google and so on), reachable
  from the API at startup. It is mandatory: static API tokens only work when
  `INVOKR_AUTH_MODE=oidc`, and the API refuses to start if discovery fails.
- **A key store** if secrets are encrypted: AWS KMS, GCP KMS or OCI Vault, with
  decrypt permission for the pods' service account.
- **A 32-byte AES key** (64 hex characters) for `INVOKR_ENCRYPTION_KEY`, for
  example from `openssl rand -hex 32`.

## Database Setup

Create the extensions as a superuser, then run the four migrations as the app
user, so the app owns every table it creates.

1. Set the server parameters and restart PostgreSQL (on RDS/Aurora, set them in
   the parameter group):

   ```text
   shared_preload_libraries = pg_cron
   cron.database_name = invokr_db
   ```

2. As a superuser, create the database, the app user and the extensions:

   ```sql
   CREATE DATABASE invokr_db;
   CREATE USER invokr WITH PASSWORD '<strong password>';
   ALTER DATABASE invokr_db OWNER TO invokr;  -- lets it CREATE SCHEMA per workspace
   \c invokr_db
   CREATE EXTENSION IF NOT EXISTS pgcrypto;
   CREATE EXTENSION IF NOT EXISTS pg_cron;
   GRANT USAGE ON SCHEMA cron TO invokr;
   ```

3. As the app user, run the four SQL files from the release's
   [`migrations/`](https://github.com/juspay/invokr/tree/v0.5.0/migrations)
   folder in this order:

   ```bash
   DB='postgresql://invokr:<pw>@<writer-host>:5432/invokr_db'
   psql "$DB" -v ON_ERROR_STOP=1 -f 20260317000000_initial.sql
   psql "$DB" -v ON_ERROR_STOP=1 -f 20260318000000_multi_tenancy.sql
   psql "$DB" -v ON_ERROR_STOP=1 -f 20260322000000_txn_based_pickup.sql
   psql "$DB" -v ON_ERROR_STOP=1 -f 20260322000001_pg_cron.sql
   ```

4. Check that it worked: `\dt public.*` lists `organizations`, `workspaces`,
   `region_heartbeats` and `region_status`, and
   `SELECT extname FROM pg_extension;` includes `pg_cron`.

On a fresh database, `20260322000000_txn_based_pickup.sql` does nothing and the
job loop in `..._pg_cron.sql` has nothing to register; both are expected.
Per-workspace tables are created later, by the API, when a workspace is
created.

## Configuration

A production install needs about a dozen variables; everything else has a
working default. Values marked sensitive must be base64 KMS ciphertext whenever
`INVOKR_SECRETS_MANAGER` isn't `no_encryption`, including the database URL.

| Variable | Read by | Set to | Notes |
| --- | --- | --- | --- |
| `INVOKR_DATABASE_URL` | API, worker | writer endpoint URL | Required. Sensitive. |
| `INVOKR_ENCRYPTION_KEY` | API, worker | 64 hex characters | Sensitive. **Silently defaults to all zeros if unset.** Must be identical on API and worker, and never changed once secrets are stored. |
| `INVOKR_SECRETS_MANAGER` | API, worker | `aws_kms`, `gcp_kms`, `oci_kms` or `no_encryption` | Default `no_encryption`. AWS uses the standard credential chain (`AWS_REGION`, IRSA). |
| `INVOKR_GCP_KMS_KEY_NAME` | API, worker | `projects/…/cryptoKeys/<k>` | Only with `gcp_kms`. |
| `INVOKR_OCI_KMS_CRYPTO_ENDPOINT`, `INVOKR_OCI_KMS_KEY_ID` | API, worker | vault crypto endpoint, key OCID | Only with `oci_kms`. |
| `INVOKR_AUTH_MODE` | API | `oidc` | Required, no default: the API won't start without it. `disabled` is for local development only. |
| `INVOKR_OIDC_ISSUER_URL` | API | issuer URL | Required with `oidc`. |
| `INVOKR_OIDC_CLIENT_ID` | API | client id | Required with `oidc`. |
| `INVOKR_OIDC_CLIENT_SECRET` | API | client secret | Sensitive. Optional, for confidential clients. |
| `INVOKR_OIDC_REDIRECT_HOST` | API | `https://<invokr-host>` | Required with `oidc`. Register `<host><INVOKR_PATH_PREFIX>/oidc/login` as the redirect URI at the identity provider. |
| `INVOKR_API_TOKEN_PREFIX` | API | `ivk_` | Must be set together with the tokens. Matched literally, so include the `_`. |
| `INVOKR_API_STATIC_TOKENS` | API | `[{"token":"<random>","principal":"<caller>"}]` | Sensitive. Callers send `Bearer ivk_<random>`. |
| `INVOKR_PATH_PREFIX` | API | `/invokr` | Code default is empty; the chart sets `/invokr`. |
| `INVOKR_MODE` | API | `both` | `api`, `dashboard` or `both`. Code default `api`; the chart sets `both`. |

Leave these as they are:

- `INVOKR_TABLE_PREFIX`: leave it empty. The `pg_cron` migration assumes tables
  without a prefix.
- `INVOKR_API_KEY`: leave it unset. It is the old shared key; while it is set
  the API accepts it and logs a warning at every startup.
- `INVOKR_AUTH_SECURE_COOKIES`: keep `true` (the default) behind HTTPS.

Worker tuning, defaults shown: `INVOKR_WORKER_MAX_CONCURRENT=50`,
`INVOKR_WORKER_POLL_INTERVAL_MS=200`, `INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC=30`,
`INVOKR_CONFIG_CACHE_TTL_SEC=60`, `INVOKR_SECRET_CACHE_TTL_SEC=300`,
`INVOKR_METRICS_PORT=9090`. `INVOKR_REAPER_CRON_EXPRESSION` (`*/15 * * * *`) is
fixed into each workspace when it is created. In the Helm chart, set it under
`apiConfigs`: only the API reads it, so under `workerConfigs` it has no effect.

## Deploy With Helm

Install the published chart with one values file. The chart's defaults are
**not** production-safe: `auth_mode` defaults to `disabled`.

### Pick a Key Store

This runbook uses AWS KMS by default; the other three need only different
settings and permissions. With any KMS, every sensitive value under `secrets`
must be base64 ciphertext made with that key. See
[Secrets Management](./secrets-management) for details on each provider.

| Key store | `configs` to set | Service account | Pods need |
| --- | --- | --- | --- |
| AWS KMS (default) | `secrets_manager: aws_kms` | annotation `eks.amazonaws.com/role-arn: <role>` (IRSA) | `kms:Decrypt` on the key |
| GCP KMS | `secrets_manager: gcp_kms`, `gcp_kms_key_name: projects/<p>/locations/<l>/keyRings/<r>/cryptoKeys/<k>` | annotation `iam.gke.io/gcp-service-account: <gsa>@<project>.iam.gserviceaccount.com` (Workload Identity) | `roles/cloudkms.cryptoKeyDecrypter` on the key |
| OCI Vault | `secrets_manager: oci_kms`, `oci_kms_crypto_endpoint: https://<vault>-crypto.kms.<region>.oraclecloud.com`, `oci_kms_key_id: ocid1.key.oc1…` | `serviceAccount.automount: true` (OKE Workload Identity; needs an Enhanced cluster) | an IAM policy letting the workload use keys in the compartment |
| No encryption | `secrets_manager: no_encryption` | nothing | nothing; keep the plain values in a Kubernetes Secret via `existingSecret` |

Encrypt each value without a trailing newline (`printf '%s'`, never `echo`). A
newline becomes part of the value, and in `INVOKR_ENCRYPTION_KEY` it breaks
every secret.

```bash
# AWS KMS
aws kms encrypt --key-id <key-id> --plaintext "$VALUE" \
  --cli-binary-format raw-in-base64-out --query CiphertextBlob --output text

# GCP KMS
printf '%s' "$VALUE" | gcloud kms encrypt --key "<key name>" \
  --plaintext-file=- --ciphertext-file=- | base64 | tr -d '\n'

# OCI Vault
oci kms crypto encrypt --endpoint "<crypto endpoint>" --key-id "<key OCID>" \
  --plaintext "$(printf '%s' "$VALUE" | base64 | tr -d '\n')" \
  --query 'data.ciphertext' --raw-output
```

### Install

1. Write `values-prod.yaml`. Each key under `configs` / `apiConfigs` /
   `workerConfigs` becomes `INVOKR_<KEY>` in that workload's ConfigMap, and each
   key under `secrets` becomes `INVOKR_<KEY>` in a Secret mounted by both
   workloads.

   ```yaml
   image:
     tag: "0.5.0"
   configs:
     path_prefix: /invokr
     db_pool_size: 20
     secrets_manager: aws_kms          # default; see Pick a Key Store
   apiConfigs:
     mode: both
     auth_mode: oidc
     oidc_issuer_url: https://<idp>/realms/<realm>
     oidc_client_id: invokr
     oidc_redirect_host: https://<invokr-host>
     api_token_prefix: ivk_
   secrets:                            # ciphertext when secrets_manager is a KMS
     database_url: <writer URL>
     encryption_key: <64 hex>
     oidc_client_secret: <client secret>
     api_static_tokens: '[{"token":"<random>","principal":"<caller>"}]'
   serviceAccount:
     annotations:
       eks.amazonaws.com/role-arn: <role with kms:Decrypt>   # aws_kms + IRSA
   api:
     ingress:
       enabled: true
       className: <class>
       hosts:
         - host: <invokr-host>
   ```

2. Leave `migration.enabled: false`. The image has no migrate-and-exit command,
   so the hook Job would hang until Helm times out.

3. Install:

   ```bash
   helm install invokr oci://ghcr.io/juspay/helm-charts/invokr --version 0.5.0 \
     --namespace invokr --create-namespace -f values-prod.yaml
   ```

4. Wait for both deployments to roll out. The chart's probes are: API liveness
   and readiness on `<path_prefix>/health`; worker liveness on `/health` and
   readiness on `/ready`, port 9090.

With external-secrets, leave `secrets` empty and set `existingSecret`; that
Secret must hold `INVOKR_DATABASE_URL` and `INVOKR_ENCRYPTION_KEY` plus any auth
secrets. Expose only the API; the worker takes no inbound traffic apart from
metrics.

## Post-Deploy Setup

Create an organization for each caller. Workspaces inside it can be created by
the caller or by you.

1. Check the API responds:

   ```bash
   curl -sf https://<invokr-host>/invokr/health
   ```

2. Create the org, using a static token from `INVOKR_API_STATIC_TOKENS`. A slug
   is 1–25 characters: lowercase letters, digits and hyphens, with no hyphen at
   either end.

   ```bash
   curl -sS -X POST https://<invokr-host>/invokr/v1/orgs \
     -H 'Authorization: Bearer ivk_<random>' \
     -H 'Content-Type: application/json' \
     -d '{"name":"<Org name>","slug":"<org-slug>"}'
   ```

3. Create a workspace in it, unless the caller creates its own:

   ```bash
   curl -sS -X POST https://<invokr-host>/invokr/v1/orgs/<org-slug>/workspaces \
     -H 'Authorization: Bearer ivk_<random>' \
     -H 'Content-Type: application/json' \
     -d '{"name":"<Workspace name>","slug":"<workspace-slug>"}'
   ```

4. Open `https://<invokr-host>/dashboard` and sign in through the identity
   provider, to confirm the OIDC login and its callback work.

Each workspace is a schema named `<org_id>_<slug>` with hyphens turned into
underscores. The API creates it with its tables and its cleanup ("reaper") job.

Callers use `https://<invokr-host>/invokr` as the base URL, send
`Authorization: Bearer ivk_<token>`, and pick the workspace with the
`x-org-id` and `x-workspace-id` headers (an id or a slug for each).

## Verify and Troubleshoot

The install is good when both health checks pass, a workspace exists, and
`pg_cron` has that workspace's reaper job.

- `curl -sf https://<invokr-host>/invokr/health` returns 200.
- Worker `/ready` on port 9090 returns 200.
- `SELECT schema_name FROM public.workspaces;` lists `<org_id>_<workspace>`.
- `SELECT jobname, schedule FROM cron.job;` shows an `invokr_<schema>_…` entry
  for that workspace.

| Symptom | Cause | Fix |
| --- | --- | --- |
| API exits: `INVOKR_AUTH_MODE is not set` | No default by design | Set `apiConfigs.auth_mode: oidc` |
| API exits: `OIDC discovery against … failed` | Issuer URL wrong or unreachable from the pod | Fix the URL or the network path to the identity provider |
| API exits: prefix and tokens `must be set together` | Only one of the pair is set | Set both `INVOKR_API_TOKEN_PREFIX` and `INVOKR_API_STATIC_TOKENS` |
| Startup error: `… is not valid base64` or `could not be decrypted` | A plaintext value while a KMS is selected, or a missing decrypt permission | Encrypt every sensitive value; grant decrypt to the service account |
| Caller gets 401 | The token is missing the `ivk_` prefix or doesn't match a configured token | Send `ivk_` + the exact `token` value |
| Workspace creation returns 404 `ORG_NOT_FOUND` | Org not created | Run the org step in Post-Deploy Setup |
| Workspace creation: `permission denied for database` | App user can't create schemas | `ALTER DATABASE invokr_db OWNER TO invokr` or `GRANT CREATE` |
| Workspace creation: `permission denied for schema cron` | Missing `pg_cron` grant | `GRANT USAGE ON SCHEMA cron TO invokr` |
| Scheduled jobs never fire, no error anywhere | `pg_cron` not preloaded, wrong `cron.database_name`, or connected to a read replica | Fix the parameters, restart, and use the writer endpoint |
| Dashboard login succeeds, then you're signed out | Secure cookies over plain HTTP | Serve over HTTPS; only set `INVOKR_AUTH_SECURE_COOKIES=false` for local HTTP |
