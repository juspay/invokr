---
id: dashboard
title: Dashboard
---

# Dashboard

Invokr includes a web-based dashboard built with [Leptos 0.7](https://leptos.dev/) compiled to WebAssembly (WASM). The dashboard provides a visual interface for browsing organizations, workspaces, jobs, executions, and attempts.

## Architecture

The dashboard is a WASM application that runs in the browser. When `INVOKR_MODE=both`, the API server serves both the REST API and the dashboard from the same process. The dashboard's WASM bundle and static assets are served from the `INVOKR_DASHBOARD_DIST_DIR` directory.

```
Browser ──→ API Server (actix-web, INVOKR_MODE=both)
              ├── /v1/*          → REST API
              ├── /dashboard/*   → Dashboard WASM + assets
              └── /metrics       → Prometheus metrics
```

### Server modes

The `INVOKR_MODE` environment variable controls what the API server serves:

| Mode | Value | Serves |
|------|-------|--------|
| API only | `api` (default) | REST API only |
| Dashboard only | `dashboard` | Dashboard only |
| Both | `both` | REST API + Dashboard (SSR) |

```bash
# API only (default)
INVOKR_MODE=api ./invokr-api

# API + Dashboard
INVOKR_MODE=both INVOKR_DASHBOARD_DIST_DIR=crates/dashboard/pkg ./invokr-api
```

## Setup

### Prerequisites

The dashboard requires `wasm-pack` and the `wasm32-unknown-unknown` Rust target:

```bash
just dashboard-setup
```

This runs:
```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack
```

:::note
The Nix development shell (`nix develop`) already includes `wasm-pack` and the `wasm32-unknown-unknown` target. If you're using Nix, you can skip `just dashboard-setup`.
:::

### Building the dashboard

```bash
# Release build (optimized, smaller WASM)
just dashboard-build

# Dev build (faster compilation, larger WASM, no optimizations)
just dashboard-build-dev
```

The build process:

1. **`wasm-pack build`** — compiles the `crates/dashboard` crate to WASM with the `hydrate` feature:
   ```bash
   cd crates/dashboard && wasm-pack build --target web --release -- --features hydrate
   ```

2. **`tailwindcss`** — generates the CSS bundle:
   ```bash
   cd crates/dashboard && tailwindcss -i input.css -o pkg/tailwind-output.css --minify
   ```

The output is placed in `crates/dashboard/pkg/`, which is the directory you point `INVOKR_DASHBOARD_DIST_DIR` to.

:::warning
The dashboard bundle **must** be built before running the API server in `both` mode. If `INVOKR_MODE=both` is set but `INVOKR_DASHBOARD_DIST_DIR` points to a non-existent or empty directory, the dashboard routes will return 404.
:::

## Running the dashboard

### Local development

```bash
just dashboard
```

This is equivalent to:

```bash
INVOKR_MODE=both INVOKR_DASHBOARD_DIST_DIR=crates/dashboard/pkg cargo run -p invokr-api
```

The API server starts on port 8080, serving both the API and the dashboard. Access the dashboard at `http://localhost:8080/dashboard/` (when `INVOKR_DASHBOARD_PATH_PREFIX=/dashboard` is set) or at the root path.

### With a path prefix

When running behind a reverse proxy or alongside other services, you typically want the dashboard under a path prefix:

```bash
INVOKR_MODE=both \
INVOKR_PATH_PREFIX=/invokr \
INVOKR_DASHBOARD_PATH_PREFIX=/dashboard \
INVOKR_API_BASE_URL=http://localhost:8080/invokr \
INVOKR_DASHBOARD_DIST_DIR=crates/dashboard/pkg \
  cargo run -p invokr-api
```

This serves:
- API at `http://localhost:8080/invokr/v1/...`
- Dashboard at `http://localhost:8080/dashboard/`

## Path prefix configuration

The dashboard uses **compile-time** environment variables that are baked into the WASM binary. Changing the prefix requires rebuilding the dashboard.

| Variable | Default | Description |
|----------|---------|-------------|
| `INVOKR_DASHBOARD_PATH_PREFIX` | *(empty)* | URL prefix for dashboard routes (e.g. `/dashboard`) |
| `INVOKR_API_BASE_URL` | *(empty)* | Full API base URL including path prefix (e.g. `http://localhost:8080/invokr`) |

:::important
`INVOKR_API_BASE_URL` must include `INVOKR_PATH_PREFIX` if it is set. For example, if `INVOKR_PATH_PREFIX=/invokr`, then `INVOKR_API_BASE_URL` should be `http://localhost:8080/invokr` (not `http://localhost:8080`).
:::

### Building with path prefix

```bash
INVOKR_DASHBOARD_PATH_PREFIX=/dashboard \
INVOKR_API_BASE_URL=http://localhost:8080/invokr \
  just dashboard-build
```

Then run the API server:

```bash
INVOKR_MODE=both \
INVOKR_PATH_PREFIX=/invokr \
INVOKR_DASHBOARD_PATH_PREFIX=/dashboard \
INVOKR_DASHBOARD_DIST_DIR=crates/dashboard/pkg \
  cargo run -p invokr-api
```

### Using .env

Since the justfile has `set dotenv-load`, you can set these in `.env`:

```env
# .env
INVOKR_PATH_PREFIX=/invokr
INVOKR_DASHBOARD_PATH_PREFIX=/dashboard
INVOKR_API_BASE_URL=http://localhost:8080/invokr
```

```bash
just dashboard-build   # bakes prefix into WASM
just dashboard         # runs with prefix
```

## Docker

When building a Docker image with `INCLUDE_DASHBOARD=true`, the Dockerfile automatically builds the dashboard WASM bundle and copies it into the image:

```bash
docker build \
  --build-arg BINARY=invokr-api \
  --build-arg INCLUDE_DASHBOARD=true \
  -t invokr-api-with-dashboard .
```

The built image sets `INVOKR_DASHBOARD_DIST_DIR=/app/dashboard-dist`, so you only need to set `INVOKR_MODE=both` at runtime:

```bash
docker run -e INVOKR_MODE=both -p 8080:8080 invokr-api-with-dashboard
```

For Docker builds with path prefix, pass the compile-time env vars as build args:

```dockerfile
# In your Dockerfile or docker-compose.yml build section:
ARG INVOKR_DASHBOARD_PATH_PREFIX=/dashboard
ARG INVOKR_API_BASE_URL=http://localhost:8080/invokr
ENV INVOKR_DASHBOARD_PATH_PREFIX=$INVOKR_DASHBOARD_PATH_PREFIX
ENV INVOKR_API_BASE_URL=$INVOKR_API_BASE_URL
```

:::note
The `docker-compose.prod.yml` already configures the dashboard with `INVOKR_PATH_PREFIX=/invokr`, `INVOKR_DASHBOARD_PATH_PREFIX=/dashboard`, and `INCLUDE_DASHBOARD=true`. See [Production Deployment](./production).
:::

## Dashboard pages

The dashboard provides the following pages:

| Page | Route | Description |
|------|-------|-------------|
| Organizations | `/dashboard/` | Lists all organizations with status badges |
| Organization detail | `/dashboard/orgs/{org_id}` | Organization details with its workspaces |
| Workspace detail | `/dashboard/orgs/{org_id}/workspaces/{workspace_id}` | Workspace details with jobs, executions, and attempts |

### Components

The dashboard is built with these reusable Leptos components:

| Component | Description |
|-----------|-------------|
| `sidebar` | Navigation sidebar with org/workspace context |
| `status_badge` | Color-coded status indicator (active, queued, running, success, failed, etc.) |
| `modal` | Modal dialog for confirmations and details |
| `loading` | Loading spinner / skeleton state |
| `confirm` | Confirmation dialog for destructive actions (cancel job, delete endpoint, etc.) |

## See also

- [Production Deployment](./production) — running the dashboard in Docker with KMS
- [Docker](./docker) — building images with `INCLUDE_DASHBOARD=true`
- [Environment Variables](../configuration/environment-variables) — `INVOKR_MODE`, `INVOKR_DASHBOARD_PATH_PREFIX`, `INVOKR_API_BASE_URL`
