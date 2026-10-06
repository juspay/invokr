---
id: secrets-management
title: Secrets Management
---

# Secrets Management

Invokr can decrypt sensitive environment variables at startup through a pluggable provider. `INVOKR_SECRETS_MANAGER` selects one:

| Value | Behaviour |
|---|---|
| `no_encryption` (default) | Variables are read verbatim. |
| `aws_kms` | Variables must be base64-encoded AWS KMS ciphertext, decrypted at startup. Region comes from the standard chain; grant `kms:Decrypt` via IRSA. |
| `gcp_kms` | Variables must be base64-encoded Cloud KMS ciphertext. Requires `INVOKR_GCP_KMS_KEY_NAME`; credentials via Application Default Credentials. |

There is a single image per workload — every provider is compiled in, so no special image tag is required.

## How it works

With a provider selected, Invokr's `SensitiveEnvReader` intercepts reads of the sensitive environment variables:

| Variable | Description |
|----------|-------------|
| `INVOKR_DATABASE_URL` | PostgreSQL connection string |
| `INVOKR_ENCRYPTION_KEY` | AES-256 key for secret encryption |
| `INVOKR_OIDC_CLIENT_SECRET` | OIDC client secret |
| `INVOKR_API_STATIC_TOKENS` | Static API tokens |
| `INVOKR_API_KEY` | Bearer token for API authentication (legacy) |

Instead of reading plaintext values, the reader hands the base64-encoded ciphertext stored in the environment variable to the provider and uses the decrypted plaintext value in memory.

This is all-or-nothing: with a provider selected, **every** variable above is treated as ciphertext. Plaintext and ciphertext cannot be mixed.

If a variable is not set at all, there is nothing to decrypt and the provider is not called for it.

## AWS KMS

### AWS region

The SDK resolves the region from the standard chain — `AWS_REGION`, `AWS_DEFAULT_REGION`, IRSA, then IMDS. Invokr does not override it. Setting `AWS_REGION` explicitly is supported but not required.

### AWS environment variables

The AWS KMS provider uses the standard AWS SDK environment variables for authentication:

| Variable | Description | Dev (LocalStack) | Production |
|----------|-------------|------------------|------------|
| `AWS_ENDPOINT_URL` | KMS endpoint URL | `http://localhost:4566` | *(omit — uses AWS)* |
| `AWS_REGION` | AWS region | `us-east-1` | *(optional — resolved from the standard chain)* |
| `AWS_ACCESS_KEY_ID` | AWS access key | `test` (any value for LocalStack) | *(omit when using IRSA)* |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key | `test` (any value for LocalStack) | *(omit when using IRSA)* |

:::tip
In production, omit `AWS_ENDPOINT_URL` to use the real AWS KMS endpoint. Prefer IRSA (IAM Roles for Service Accounts) over static credentials, and grant the role `kms:Decrypt` on your key.
:::

## Local development with LocalStack

Invokr uses [LocalStack](https://localstack.cloud/) to emulate AWS KMS locally. The `docker-compose.yml` includes a LocalStack service under the `kms` profile.

### Step 1: Start LocalStack

```bash
just kms-up
```

This starts the LocalStack container with KMS enabled on port 4566.

### Step 2: Create a KMS key and encrypt values

```bash
just kms-init
```

This runs `scripts/kms-init.sh`, which:

1. Creates a KMS key on LocalStack with the description "Invokr dev encryption key"
2. Creates an alias `alias/invokr-dev` for the key
3. Saves the key ID to `.kms-key-id`
4. Encrypts the sensitive env vars it sets:
   - `INVOKR_DATABASE_URL`
   - `INVOKR_API_KEY`
   - `INVOKR_ENCRYPTION_KEY`
5. Writes a complete `.env.kms` file with encrypted values + plaintext non-sensitive config + AWS config

The script reads plaintext values from the current environment or falls back to defaults:

```bash
# Defaults used by kms-init.sh:
INVOKR_DATABASE_URL=postgresql://invokr:invokr@localhost:5434/invokr_db
INVOKR_API_KEY=dev-api-key
INVOKR_ENCRYPTION_KEY=0000000000000000000000000000000000000000000000000000000000000000
```

To encrypt custom values, set them before running `just kms-init`:

```bash
INVOKR_API_KEY=my-secret-api-key INVOKR_DATABASE_URL=postgresql://user:pass@db:5432/mydb just kms-init
```

### Step 3: Run with the AWS KMS provider

```bash
just kms-dev
```

This starts the API server and worker using the `.env.kms` file, which selects `INVOKR_SECRETS_MANAGER=aws_kms`. The script verifies `.env.kms` exists before starting.

:::note
`just kms-dev` unsets the plaintext `INVOKR_DATABASE_URL`, `INVOKR_API_KEY`, and `INVOKR_ENCRYPTION_KEY` from the environment before starting, ensuring only the KMS-encrypted versions are used.
:::

### Encrypting additional values

To encrypt an arbitrary plaintext value with the LocalStack KMS key:

```bash
just kms-encrypt "my secret value"
```

This uses `scripts/kms-encrypt.sh`, which reads the key ID from `.kms-key-id` (created by `kms-init`). You can also specify a key ID explicitly:

```bash
just kms-encrypt -k <key-id> "my secret value"
```

The script outputs the base64-encoded KMS ciphertext, which you can paste into your `.env.kms` or `.env.prod.kms` file.

### Generated .env.kms file

The `kms-init.sh` script generates a file like this:

```bash
# Generated by scripts/kms-init.sh — AWS KMS dev environment

INVOKR_SECRETS_MANAGER=aws_kms

# Encrypted values (base64-encoded KMS ciphertext)
INVOKR_DATABASE_URL=AQICAHh...base64...
INVOKR_API_KEY=AQICAHh...base64...
INVOKR_ENCRYPTION_KEY=AQICAHh...base64...

# Non-sensitive values (plaintext)
INVOKR_LISTEN_ADDR=0.0.0.0:8080
INVOKR_DB_POOL_SIZE=20
INVOKR_WORKER_MAX_CONCURRENT=50
INVOKR_WORKER_POLL_INTERVAL_MS=200
INVOKR_CONFIG_CACHE_TTL_SEC=60
INVOKR_SECRET_CACHE_TTL_SEC=300
INVOKR_WORKER_SHUTDOWN_TIMEOUT_SEC=30

# AWS / LocalStack
AWS_ENDPOINT_URL=http://localhost:4566
AWS_REGION=us-east-1
AWS_ACCESS_KEY_ID=test
AWS_SECRET_ACCESS_KEY=test
```

### Stopping LocalStack

```bash
just kms-down
```

## Production AWS KMS setup

In production, you use a real AWS KMS key instead of LocalStack.

### 1. Create a KMS key in AWS

```bash
aws kms create-key \
  --description "Invokr production encryption key" \
  --region us-east-1

# Note the KeyId from the output

aws kms create-alias \
  --alias-name alias/invokr-prod \
  --target-key-id <key-id>
```

### 2. Encrypt your sensitive values

```bash
aws kms encrypt \
  --key-id <key-id> \
  --plaintext "postgresql://user:pass@prod-db:5432/invokr" \
  --cli-binary-format raw-in-base64-out \
  --query 'CiphertextBlob' \
  --output text
```

Repeat for every sensitive variable you set — `INVOKR_ENCRYPTION_KEY`, `INVOKR_OIDC_CLIENT_SECRET`, `INVOKR_API_STATIC_TOKENS` and the legacy `INVOKR_API_KEY`.

### 3. Configure the environment

Set the following environment variables in your production deployment:

```bash
INVOKR_SECRETS_MANAGER=aws_kms

# Encrypted values (base64-encoded KMS ciphertext from step 2)
INVOKR_DATABASE_URL=<encrypted-db-url>
INVOKR_ENCRYPTION_KEY=<encrypted-encryption-key>
INVOKR_OIDC_CLIENT_SECRET=<encrypted-oidc-client-secret>
INVOKR_API_STATIC_TOKENS=<encrypted-static-tokens>

# AWS configuration (omit AWS_ENDPOINT_URL for real AWS)
# AWS_REGION is optional — the SDK resolves it from the standard chain
# Prefer IRSA over explicit credentials:
# AWS_ACCESS_KEY_ID=<your-access-key>
# AWS_SECRET_ACCESS_KEY=<your-secret-key>
```

:::danger
**Do not** set `AWS_ENDPOINT_URL` in production — this would redirect KMS calls to a non-AWS endpoint. The variable should only be set for LocalStack development.
:::

## GCP KMS

Select the provider and point it at a key:

```bash
INVOKR_SECRETS_MANAGER=gcp_kms
INVOKR_GCP_KMS_KEY_NAME=projects/<project>/locations/<location>/keyRings/<ring>/cryptoKeys/<key>
```

`INVOKR_GCP_KMS_KEY_NAME` is required for this provider and must be the full Cloud KMS key resource name. Credentials are resolved through Application Default Credentials — Workload Identity on GKE, or `GOOGLE_APPLICATION_CREDENTIALS` pointing at a service account key elsewhere. The identity needs the `cloudkms.cryptoKeyVersions.useToDecrypt` permission on the key.

Sensitive variables are base64-encoded Cloud KMS ciphertext, exactly as with AWS KMS.

## The docker-prod.sh script

The `scripts/docker-prod.sh` script automates the full prod-like AWS KMS setup in Docker. It:

1. Builds all images
2. Starts PostgreSQL and LocalStack
3. Runs database migrations
4. Creates a KMS key on LocalStack via `awslocal`
5. Encrypts `INVOKR_DATABASE_URL` (using Docker-internal hostname `postgres:5432`), `INVOKR_API_KEY`, and `INVOKR_ENCRYPTION_KEY`
6. Writes `.env.prod.kms` with the encrypted values
7. Starts the API server and worker
8. Waits for health checks

The API server and worker in `docker-compose.prod.yml` reference `.env.prod.kms` via `env_file`:

```yaml
env_file:
  - path: .env.prod.kms
    required: false
```

## Key management

| Aspect | Dev (LocalStack) | Production |
|--------|-------------------|------------|
| Key creation | `just kms-init` | `aws kms create-key` |
| Key alias | `alias/invokr-dev` | `alias/invokr-prod` |
| Key ID storage | `.kms-key-id` file | AWS KMS console / IaC |
| Endpoint | `http://localhost:4566` | AWS (default endpoint) |
| Credentials | `test` / `test` | IRSA or explicit keys |
| Key rotation | Manual (re-run `kms-init`) | Enable automatic key rotation in AWS |

:::tip
Enable [automatic key rotation](https://docs.aws.amazon.com/kms/latest/developerguide/rotate-keys.html) on your production KMS key for an additional layer of security. Invokr will transparently handle rotated keys — the key ID in the ciphertext blob identifies which key to use for decryption.
:::

## See also

- [Production Deployment](./production) — full prod-like Docker setup
- [Docker](./docker) — Dockerfile and build arguments
- [Environment Variables](../configuration/environment-variables) — all environment variables including `INVOKR_SECRETS_MANAGER`
