# Dashboard Runtime-Configurable Prefix Implementation Plan

> **Historical document — predates the rename.** It was written while the
> service was called `kronos` and the environment variables were prefixed
> `TE_`. Both are gone: the service is **Invokr** and the variables are
> `INVOKR_*` (see `.env.example`). Read this for the reasoning, not for the
> names.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `TE_DASHBOARD_PATH_PREFIX` and `TE_API_BASE_URL` configurable at container start so one `kronos-dashboard` image works under any URL prefix or against any API URL without rebuild.

**Architecture:** The Leptos+WASM SPA reads its prefix and API URL from `<meta>` tags in `index.html` at boot instead of from `option_env!`. The Docker image ships a templated `index.html.template`; an entrypoint script runs `envsubst` at container start to produce the final `index.html`. Asset URLs are made portable via a templated `<base href>` tag plus `trunk build --public-url ./`.

**Tech Stack:** Leptos 0.7 (CSR), `web-sys` for DOM access, Trunk for WASM build, nginx:alpine for serving, `envsubst` (from `gettext`, included in `nginx:alpine`) for templating.

---

## File Structure

**New files:**
- `crates/dashboard/index.html.template` — docker-only HTML with `<base>` and `<meta>` placeholders
- `crates/dashboard/src/runtime_config.rs` — single module that reads + normalizes runtime config
- `docker/dashboard/docker-entrypoint.sh` — runs `envsubst`, then nginx

**Modified files:**
- `crates/dashboard/Cargo.toml` — add `web-sys` features for `Document` / `Element`
- `crates/dashboard/src/main.rs` — declare new `runtime_config` module
- `crates/dashboard/src/app.rs` — replace compile-time prefix with runtime read
- `crates/dashboard/src/api/client.rs` — replace compile-time API URL with runtime read
- `Dockerfile.dashboard` — drop build-args, use template, set entrypoint
- `.github/workflows/release.yml` — drop `TE_DASHBOARD_PATH_PREFIX`/`TE_API_BASE_URL` build-args
- `justfile` — drop conditional `--public-url` from dashboard recipes
- `.env.example` — clarify these are container-runtime, not build-time
- `README.md` — replace "compile-time" wording with "runtime via container env"

**Unchanged:**
- `crates/dashboard/index.html` — stays as the dev/source-tree version (no placeholders)
- `docker/dashboard/nginx.conf` — `<base>` handles asset URLs, no nginx changes needed
- `crates/common/src/config.rs` — API server already runtime, untouched
- `crates/dashboard/src/components/sidebar.rs`, `pages/*.rs` — they use `prefixed()`, signature unchanged

---

## Task 1: Add `web-sys` features needed for meta-tag reads

**Files:**
- Modify: `crates/dashboard/Cargo.toml`

The runtime config module needs `Document::query_selector` and `Element::get_attribute`. The current `web-sys` features list (`Window`, `Location`) is too narrow — `Document` and `Element` aren't enabled.

- [ ] **Step 1: Add features**

In `crates/dashboard/Cargo.toml`, change line 15:

```toml
web-sys = { version = "0.3", features = ["Window", "Location", "Document", "Element"] }
```

- [ ] **Step 2: Verify it compiles**

Run: `cd crates/dashboard && cargo check --target wasm32-unknown-unknown`
Expected: `Finished` with no errors. Compile time may be longer the first time as new web-sys features pull in extra bindings.

- [ ] **Step 3: Commit**

```bash
git add crates/dashboard/Cargo.toml crates/dashboard/Cargo.lock
git commit -m "deps(dashboard): add web-sys Document and Element features"
```

---

## Task 2: Create `runtime_config` module

**Files:**
- Create: `crates/dashboard/src/runtime_config.rs`
- Modify: `crates/dashboard/src/main.rs`

A single place that reads `<meta>` tags, normalizes the values, and exposes them to the rest of the app. Centralizing this means the rules ("trim trailing slash", "treat literal `${...}` as unset") live in one file.

- [ ] **Step 1: Create the module**

Create `crates/dashboard/src/runtime_config.rs`:

```rust
//! Runtime configuration read from <meta> tags in index.html.
//!
//! The Docker image's entrypoint substitutes env vars into index.html via
//! envsubst before nginx starts, so by the time the WASM runs, the meta tags
//! carry the operator's values. In dev (trunk serve), index.html has no
//! such tags, so reads return empty strings — i.e. "no prefix, derive API
//! URL from window.location" — which matches today's no-prefix dev workflow.

use web_sys::window;

/// Read a meta tag's `content` attribute by name. Returns an empty string
/// when the tag is missing, the attribute is missing, or the value is a
/// literal `${VAR}` placeholder (unsubstituted in dev).
fn read_meta(name: &str) -> String {
    let Some(doc) = window().and_then(|w| w.document()) else {
        return String::new();
    };
    let selector = format!("meta[name=\"{name}\"]");
    let Ok(Some(el)) = doc.query_selector(&selector) else {
        return String::new();
    };
    let value = el.get_attribute("content").unwrap_or_default();
    if value.starts_with("${") {
        // Unsubstituted placeholder — treat as unset.
        return String::new();
    }
    value
}

/// Normalize a path prefix: empty or bare "/" → "" ; otherwise ensure exactly
/// one leading "/" and no trailing "/". Same rules as the API server's
/// `TE_PATH_PREFIX` normalization in crates/common/src/config.rs.
fn normalize_prefix(raw: &str) -> String {
    let trimmed = raw.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// Path prefix the dashboard is served under (e.g. "/dashboard" or "").
pub fn path_prefix() -> String {
    normalize_prefix(&read_meta("te-path-prefix"))
}

/// Full API base URL (e.g. "http://localhost:8080/kronos"), or empty
/// to fall back to deriving from window.location.
pub fn api_base_url() -> String {
    read_meta("te-api-base-url").trim_end_matches('/').to_string()
}
```

- [ ] **Step 2: Declare the module**

Modify `crates/dashboard/src/main.rs`. Replace the current contents (15 lines) with:

```rust
mod api;
mod app;
mod components;
mod pages;
mod runtime_config;

use app::App;
use leptos::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Debug);

    mount_to_body(App);
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd crates/dashboard && cargo check --target wasm32-unknown-unknown`
Expected: `Finished` with no errors. (Module is unused so far — it'll be referenced in Task 3 and 4. The compiler may warn about unused code; that's fine.)

- [ ] **Step 4: Commit**

```bash
git add crates/dashboard/src/runtime_config.rs crates/dashboard/src/main.rs
git commit -m "feat(dashboard): add runtime_config module reading meta tags"
```

---

## Task 3: Switch `app.rs` to runtime path prefix

**Files:**
- Modify: `crates/dashboard/src/app.rs`

Replace the compile-time `PATH_PREFIX` constant (and its compile-time validation block) with a one-shot runtime read at app entry. `<Router base=...>` requires `&'static str`, so the `String` is converted via `Box::leak(s.into_boxed_str())` — bounded one-time leak, fine for a value that lives until the tab closes.

- [ ] **Step 1: Rewrite the file**

Replace the entire contents of `crates/dashboard/src/app.rs` with:

```rust
use leptos::prelude::*;
use leptos_router::{
    components::{Route, Router, Routes},
    path,
};

use crate::components::sidebar::Sidebar;
use crate::pages::{
    org_detail::OrgDetailPage,
    organizations::OrganizationsPage,
    workspace_detail::WorkspaceDetailPage,
};
use crate::runtime_config;

/// Cached path prefix string. Computed once on first access from the meta tag,
/// then leaked to obtain a `&'static str` for `<Router base=...>`.
fn path_prefix_static() -> &'static str {
    use std::sync::OnceLock;
    static PREFIX: OnceLock<&'static str> = OnceLock::new();
    PREFIX.get_or_init(|| {
        let owned = runtime_config::path_prefix();
        Box::leak(owned.into_boxed_str())
    })
}

/// Prepend the dashboard path prefix to an internal route path.
pub fn prefixed(path: &str) -> String {
    format!("{}{path}", path_prefix_static())
}

#[component]
pub fn App() -> impl IntoView {
    let base = path_prefix_static();

    view! {
        <Router base=base>
            <div class="flex min-h-screen">
                <Sidebar />
                <main class="flex-1 p-8">
                    <Routes fallback=|| "Page not found.">
                        <Route path=path!("/") view=OrganizationsPage />
                        <Route path=path!("/orgs/:org_id") view=OrgDetailPage />
                        <Route path=path!("/orgs/:org_id/workspaces/:workspace_id") view=WorkspaceDetailPage />
                    </Routes>
                </main>
            </div>
        </Router>
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd crates/dashboard && cargo check --target wasm32-unknown-unknown`
Expected: `Finished` with no errors. The previous `option_env!`-based code is gone; `prefixed()` keeps the same `&str -> String` signature so `sidebar.rs` and pages do not need updates.

- [ ] **Step 3: Commit**

```bash
git add crates/dashboard/src/app.rs
git commit -m "refactor(dashboard): read path prefix from meta tag at runtime"
```

---

## Task 4: Switch `api/client.rs` to runtime API base URL

**Files:**
- Modify: `crates/dashboard/src/api/client.rs:8-22`

`base_url()` currently reads `TE_API_BASE_URL` via `option_env!`. Replace with a runtime meta-tag read. The existing `window.location`-based fallback (used in dev) stays.

- [ ] **Step 1: Replace the function**

In `crates/dashboard/src/api/client.rs`, find the existing `base_url()` (lines 8–22):

```rust
fn base_url() -> String {
    // If TE_API_BASE_URL is set at compile time, use it directly.
    // e.g. TE_API_BASE_URL=http://localhost:8080/kronos
    if let Some(url) = option_env!("TE_API_BASE_URL") {
        return url.trim_end_matches('/').to_string();
    }

    let location = web_sys::window().unwrap().location();
    let host = location.host().unwrap_or_default();
    if host.contains("3000") {
        String::new()
    } else {
        format!("http://{host}")
    }
}
```

Replace it with:

```rust
fn base_url() -> String {
    // If a runtime API base URL is configured (via meta tag set by the
    // container entrypoint), use it directly.
    let configured = crate::runtime_config::api_base_url();
    if !configured.is_empty() {
        return configured;
    }

    // Otherwise derive from window.location:
    // - dev (trunk serve on :3000): empty string → trunk's [[proxy]] handles /v1
    // - same-origin prod: scheme + host (no path), assuming API at root
    let location = web_sys::window().unwrap().location();
    let host = location.host().unwrap_or_default();
    if host.contains("3000") {
        String::new()
    } else {
        format!("http://{host}")
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd crates/dashboard && cargo check --target wasm32-unknown-unknown`
Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/dashboard/src/api/client.rs
git commit -m "refactor(dashboard): read API base URL from meta tag at runtime"
```

---

## Task 5: Verify dev workflow still works end-to-end

This task runs no edits — it confirms the previous four tasks didn't regress the dev path. The dev workflow is `trunk serve` with no env vars, expecting prefix = "" and API base derived from `window.location`.

- [ ] **Step 1: Build the WASM bundle for dev**

Run: `cd crates/dashboard && trunk build`
Expected: `Built` line at the end, no errors. `dist/index.html` exists. Asset filenames look like `dashboard-<hash>.js`, `dashboard-<hash>_bg.wasm`.

- [ ] **Step 2: Inspect the built `dist/index.html`**

Run: `grep -E '<meta name="te-|<base ' crates/dashboard/dist/index.html || echo "no template tags (expected for dev)"`
Expected: `no template tags (expected for dev)` — the source `index.html` has no template tags, so the dev build has none either. Dashboard will run with empty prefix and same-origin API URL.

- [ ] **Step 3: Spot-check it loads in a browser**

Run: `cd crates/dashboard && trunk serve` (leave running)
In a browser, open http://localhost:3000/. The dashboard should render the Organizations page (assuming an API server is running, otherwise the data fetch will error — that's fine, we're checking that the WASM mounts and the router treats `/` as base, not that the API is reachable).

Stop trunk with Ctrl-C.

- [ ] **Step 4: No commit**

Verification only.

---

## Task 6: Add `index.html.template` for the Docker build

**Files:**
- Create: `crates/dashboard/index.html.template`

The template duplicates `index.html` plus three lines: a `<base>` tag and two `<meta>` tags carrying placeholders the entrypoint will substitute. We keep the source `index.html` clean so dev (which has no envsubst) is unaffected. The Dockerfile will copy this template over `index.html` before invoking trunk.

- [ ] **Step 1: Create the file**

Create `crates/dashboard/index.html.template` with:

```html
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <base href="${TE_DASHBOARD_PATH_PREFIX}/">
    <meta name="te-path-prefix"  content="${TE_DASHBOARD_PATH_PREFIX}">
    <meta name="te-api-base-url" content="${TE_API_BASE_URL}">
    <title>Kronos Dashboard</title>
    <link data-trunk rel="rust" data-wasm-opt="z" />
    <script src="https://cdn.tailwindcss.com"></script>
    <style>
        body { font-family: 'Inter', system-ui, -apple-system, sans-serif; }
    </style>
</head>
<body class="bg-gray-50 text-gray-900 min-h-screen">
</body>
</html>
```

- [ ] **Step 2: Confirm Trunk preserves the lines through a build**

This is a one-time sanity check that Trunk does not strip or reformat our `<base>`/`<meta>` lines. Run from repo root:

```bash
cd crates/dashboard
cp index.html index.html.dev.bak
cp index.html.template index.html
trunk build --release --public-url ./
grep -E '<meta name="te-|<base ' dist/index.html
mv index.html.dev.bak index.html
```

Expected: three matching lines printed (the `<base>`, `te-path-prefix` meta, `te-api-base-url` meta), each with the literal `${...}` placeholders intact, and asset URLs in `dist/index.html` use `./` (e.g. `<link rel="modulepreload" href="./dashboard-<hash>.js">`).

If the lines are missing or mangled, stop and investigate before continuing.

- [ ] **Step 3: Clean up the test build**

```bash
rm -rf crates/dashboard/dist
```

- [ ] **Step 4: Commit**

```bash
git add crates/dashboard/index.html.template
git commit -m "feat(dashboard): add index.html.template for runtime envsubst"
```

---

## Task 7: Add `docker-entrypoint.sh`

**Files:**
- Create: `docker/dashboard/docker-entrypoint.sh`

POSIX shell script: normalize the prefix, run `envsubst` on the template to produce `index.html`, then exec nginx. Single-quoted variable list scopes substitution to the two names — any literal `${...}` inside emitted JS or CSS is left alone.

- [ ] **Step 1: Create the script**

Create `docker/dashboard/docker-entrypoint.sh`:

```sh
#!/bin/sh
set -eu

# Normalize prefix: strip a single trailing "/" so "/" or "/dashboard/" both
# behave like "" or "/dashboard". The envsubst output uses "${PREFIX}/" so the
# trailing slash always comes from the substitution itself.
PREFIX="${TE_DASHBOARD_PATH_PREFIX:-}"
PREFIX="${PREFIX%/}"
export TE_DASHBOARD_PATH_PREFIX="$PREFIX"

# Default API base URL to empty so the WASM falls back to window.location.
export TE_API_BASE_URL="${TE_API_BASE_URL:-}"

TEMPLATE=/usr/share/nginx/html/index.html.template
OUT=/usr/share/nginx/html/index.html

if [ ! -f "$TEMPLATE" ]; then
    echo "docker-entrypoint: $TEMPLATE not found" >&2
    exit 1
fi

envsubst '${TE_DASHBOARD_PATH_PREFIX} ${TE_API_BASE_URL}' < "$TEMPLATE" > "$OUT"

echo "docker-entrypoint: rendered index.html with TE_DASHBOARD_PATH_PREFIX='$PREFIX' TE_API_BASE_URL='$TE_API_BASE_URL'"

exec "$@"
```

- [ ] **Step 2: Make it executable**

```bash
chmod +x docker/dashboard/docker-entrypoint.sh
```

- [ ] **Step 3: Commit**

```bash
git add docker/dashboard/docker-entrypoint.sh
git commit -m "feat(dashboard): add envsubst entrypoint for runtime config"
```

---

## Task 8: Rewrite `Dockerfile.dashboard`

**Files:**
- Modify: `Dockerfile.dashboard`

Drop the `ARG`/`ENV` for the two compile-time vars. Always build with `--public-url ./`. Copy the template over `index.html` just before `trunk build`, then move the built `dist/index.html` to `dist/index.html.template` in the serve stage. Wire the entrypoint.

- [ ] **Step 1: Replace the file**

Replace the entire contents of `Dockerfile.dashboard` with:

```dockerfile
# ── Build stage ────────────────────────────────────────────────
FROM rust:bookworm AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Install wasm target and Trunk
RUN rustup target add wasm32-unknown-unknown
RUN cargo install trunk --locked

WORKDIR /app
COPY . .

# Use the templated index.html for the docker build (placeholders for
# <base href> and runtime config <meta> tags). The source crates/dashboard/
# index.html stays clean for dev.
WORKDIR /app/crates/dashboard
RUN cp index.html.template index.html

# --public-url ./ emits relative asset URLs; combined with <base href> in
# the template, the same bundle works under any URL prefix at runtime.
RUN trunk build --release --public-url ./

# ── Serve stage ───────────────────────────────────────────────
FROM nginx:alpine

# envsubst lives in the gettext package; verify it's present (nginx:alpine
# ships it). If not, install: apk add --no-cache gettext.
RUN command -v envsubst >/dev/null 2>&1 || apk add --no-cache gettext

COPY docker/dashboard/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=builder /app/crates/dashboard/dist /usr/share/nginx/html

# The built index.html contains literal ${TE_DASHBOARD_PATH_PREFIX} placeholders
# and would be confusing if served as-is. Move it aside so the entrypoint
# always renders a fresh index.html from the template.
RUN mv /usr/share/nginx/html/index.html /usr/share/nginx/html/index.html.template

COPY docker/dashboard/docker-entrypoint.sh /docker-entrypoint.sh
RUN chmod +x /docker-entrypoint.sh

EXPOSE 80
ENTRYPOINT ["/docker-entrypoint.sh"]
CMD ["nginx", "-g", "daemon off;"]
```

- [ ] **Step 2: Build the image**

Run from repo root:
```bash
docker build -f Dockerfile.dashboard -t kronos-dashboard:test .
```
Expected: build succeeds, ending with `naming to docker.io/library/kronos-dashboard:test`.

- [ ] **Step 3: Commit**

```bash
git add Dockerfile.dashboard
git commit -m "feat(dashboard): build runtime-configurable docker image"
```

---

## Task 9: Verify the docker image runtime-templates correctly

This task runs no edits — it validates the image behaves as designed under three configurations. Each step starts a container, checks the served `index.html`, and stops the container.

- [ ] **Step 1: Run with no env (default)**

```bash
docker run -d --name kronos-dash-test -p 18080:80 kronos-dashboard:test
sleep 1
curl -sf http://localhost:18080/ | grep -E '<base |<meta name="te-'
docker logs kronos-dash-test | grep docker-entrypoint
docker rm -f kronos-dash-test
```

Expected output snippet:
```
<base href="/">
<meta name="te-path-prefix"  content="">
<meta name="te-api-base-url" content="">
```
And the entrypoint log line: `docker-entrypoint: rendered index.html with TE_DASHBOARD_PATH_PREFIX='' TE_API_BASE_URL=''`

- [ ] **Step 2: Run with prefix and API URL set**

```bash
docker run -d --name kronos-dash-test -p 18080:80 \
  -e TE_DASHBOARD_PATH_PREFIX=/dashboard \
  -e TE_API_BASE_URL=http://api.example.com/kronos \
  kronos-dashboard:test
sleep 1
curl -sf http://localhost:18080/ | grep -E '<base |<meta name="te-'
docker rm -f kronos-dash-test
```

Expected:
```
<base href="/dashboard/">
<meta name="te-path-prefix"  content="/dashboard">
<meta name="te-api-base-url" content="http://api.example.com/kronos">
```

- [ ] **Step 3: Run with trailing-slash prefix (normalization check)**

```bash
docker run -d --name kronos-dash-test -p 18080:80 \
  -e TE_DASHBOARD_PATH_PREFIX=/dashboard/ \
  kronos-dashboard:test
sleep 1
curl -sf http://localhost:18080/ | grep '<base '
docker rm -f kronos-dash-test
```

Expected: `<base href="/dashboard/">` (single trailing slash, not `//`). Confirms the entrypoint stripped the input's trailing slash.

- [ ] **Step 4: Browser smoke test under prefix**

This is a manual check — the previous steps confirm the rendered HTML is correct, but we want to see the WASM mount.

```bash
docker run -d --name kronos-dash-test -p 18080:80 \
  -e TE_DASHBOARD_PATH_PREFIX=/dashboard \
  kronos-dashboard:test
```

Open http://localhost:18080/dashboard/ in a browser. Open DevTools → Network. Confirm:
- The WASM file is fetched from `/dashboard/dashboard-<hash>_bg.wasm` (not `/dashboard-<hash>_bg.wasm`).
- The dashboard renders (Organizations page; data fetch will fail since no API is running — fine).
- Click into a route; URL becomes `/dashboard/orgs/...`.

Then visit http://localhost:18080/dashboard (no trailing slash). The browser will request `/dashboard`, nginx serves `index.html` (per `try_files`), and the `<base href="/dashboard/">` makes relative asset URLs resolve to `/dashboard/...`. WASM mounts. (URL stays at `/dashboard` in the address bar; the router base is `/dashboard` so it still matches.)

```bash
docker rm -f kronos-dash-test
```

- [ ] **Step 5: No commit**

Verification only.

---

## Task 10: Drop build-args from CI workflow

**Files:**
- Modify: `.github/workflows/release.yml`

The `build-and-push-dashboard` job currently passes `vars.TE_DASHBOARD_PATH_PREFIX` and `vars.TE_API_BASE_URL` as build-args. These are no longer accepted by the Dockerfile.

- [ ] **Step 1: Locate and remove the build-args**

In `.github/workflows/release.yml`, find the `build-and-push-dashboard` job's `Build and push` step. Remove these three lines from its `with:` block:

```yaml
          build-args: |
            TE_DASHBOARD_PATH_PREFIX=${{ vars.TE_DASHBOARD_PATH_PREFIX }}
            TE_API_BASE_URL=${{ vars.TE_API_BASE_URL }}
```

The remaining `with:` keys (`context`, `file`, `push`, `tags`, `labels`, `cache-from`, `cache-to`) stay.

- [ ] **Step 2: Verify with a syntax check**

Run: `git diff .github/workflows/release.yml`
Confirm only the four lines (the `build-args:` key plus its three list items) are removed; nothing else.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(dashboard): drop compile-time build-args, image is now runtime-configurable"
```

---

## Task 11: Simplify `justfile` and update `.env.example`

**Files:**
- Modify: `justfile:336-344`
- Modify: `.env.example:39-42`

The justfile currently has conditional `--public-url` plumbing for the dev recipe. Dev never uses a prefix, so this can collapse. `.env.example` mentions "compile-time env vars for WASM build" — that's no longer accurate; these are container-runtime now.

- [ ] **Step 1: Simplify dashboard justfile recipes**

In `justfile`, replace the block at lines 336–344:

```
# Build the dashboard (requires trunk and wasm32 target)
# Set TE_DASHBOARD_PATH_PREFIX and TE_API_BASE_URL env vars for prefix support
dashboard-build:
    cd crates/dashboard && trunk build --release {{ if env("TE_DASHBOARD_PATH_PREFIX", "") != "" { "--public-url " + env("TE_DASHBOARD_PATH_PREFIX", "") } else { "" } }}

# Run the dashboard dev server (port 3000, proxies API to 8080)
# Set TE_DASHBOARD_PATH_PREFIX and TE_API_BASE_URL env vars for prefix support
dashboard:
    cd crates/dashboard && trunk serve {{ if env("TE_DASHBOARD_PATH_PREFIX", "") != "" { "--public-url " + env("TE_DASHBOARD_PATH_PREFIX", "") } else { "" } }}
```

with:

```
# Build the dashboard (requires trunk and wasm32 target).
# Dev/source builds always serve at root; the docker image is what carries
# runtime prefix support — see Dockerfile.dashboard.
dashboard-build:
    cd crates/dashboard && trunk build --release

# Run the dashboard dev server (port 3000, proxies API to 8080).
dashboard:
    cd crates/dashboard && trunk serve
```

- [ ] **Step 2: Update `.env.example`**

In `.env.example`, replace lines 39–42:

```
# Dashboard (compile-time env vars for WASM build)
# TE_DASHBOARD_PATH_PREFIX=/dashboard
# TE_API_BASE_URL must include TE_PATH_PREFIX if set (e.g. /kronos)
# TE_API_BASE_URL=http://localhost:8080/kronos
```

with:

```
# Dashboard — runtime env vars for the kronos-dashboard docker image.
# These are read by the container entrypoint (envsubst) at start; no rebuild
# is needed when changing them. Not used by `just dashboard` (dev runs at root).
# TE_DASHBOARD_PATH_PREFIX=/dashboard
# TE_API_BASE_URL must include TE_PATH_PREFIX if set (e.g. /kronos)
# TE_API_BASE_URL=http://localhost:8080/kronos
```

- [ ] **Step 3: Verify justfile parses**

Run: `just --list | grep dashboard`
Expected: lists `dashboard`, `dashboard-build`, `dashboard-setup` with no parse errors.

- [ ] **Step 4: Commit**

```bash
git add justfile .env.example
git commit -m "chore(dashboard): drop compile-time prefix plumbing from dev tooling"
```

---

## Task 12: Update `README.md` path-prefix section

**Files:**
- Modify: `README.md:498-524`

The "Dashboard — uses compile-time env vars (baked into the WASM binary)" section is wrong now. Rewrite it.

- [ ] **Step 1: Replace the dashboard-prefix subsection**

In `README.md`, find the block from line 498 ("Dashboard — uses compile-time...") through line 524 ("Without these variables, everything works at the root path as before."). Replace with:

```markdown
**Dashboard** — set on the `kronos-dashboard` container at runtime via env vars (no rebuild needed):

| Variable | Default | Description |
|----------|---------|-------------|
| `TE_DASHBOARD_PATH_PREFIX` | *(empty)* | URL prefix for dashboard routes (e.g. `/dashboard`) |
| `TE_API_BASE_URL` | *(empty)* | Full API base URL including prefix (e.g. `http://localhost:8080/kronos`); empty falls back to same-origin |

The container's entrypoint runs `envsubst` over `index.html.template` at start, so changing these values requires only a container restart. Asset URLs are made portable via a templated `<base href>` plus `trunk build --public-url ./` — the same image works under any prefix.

```bash
# Run the dashboard image with a prefix and API URL:
docker run -p 8081:80 \
  -e TE_DASHBOARD_PATH_PREFIX=/dashboard \
  -e TE_API_BASE_URL=http://localhost:8080/kronos \
  ghcr.io/juspay/kronos-dashboard:latest
```

For local dev (`just dashboard`), the dashboard always runs at root with API auto-derived from `window.location` — these env vars are not consulted.

Without these variables, the dashboard image works at the root path against a same-origin API.
```

- [ ] **Step 2: Verify the section reads cleanly**

Run: `sed -n '485,535p' README.md`
Skim the output; confirm there is one consistent voice from "Path prefix" heading through the monitoring note, and no leftover "compile-time" or "baked into the WASM binary" wording.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: dashboard prefix is now runtime-configurable via container env"
```

---

## Final Self-Review Checklist

After all 12 tasks land, verify:

- [ ] `grep -r 'option_env!' crates/dashboard/src/` returns nothing (compile-time reads fully removed).
- [ ] `git diff origin/main...HEAD --stat` shows only the files listed in "File Structure" above (no surprise edits).
- [ ] The image built at the end of Task 8 ran successfully under all three configurations in Task 9.
- [ ] `cd crates/dashboard && trunk serve` still works as before (Task 5).

If all four pass, the work is complete and the branch is ready to push and PR against `main`.
