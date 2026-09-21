# Runtime-configurable dashboard prefix and API base URL

> **Historical document — predates the rename.** It was written while the
> service was called `kronos` and the environment variables were prefixed
> `TE_`. Both are gone: the service is **Invokr** and the variables are
> `INVOKR_*` (see `.env.example`). Read this for the reasoning, not for the
> names.

**Date:** 2026-04-26
**Branch:** `dashboard-docker`
**Status:** Approved, pending implementation plan

## Problem

The dashboard is a Leptos+WASM SPA. Two values are currently baked into the WASM at
build time via `option_env!`:

- `TE_DASHBOARD_PATH_PREFIX` — drives `<Router base=...>` and sidebar link prefixing
- `TE_API_BASE_URL` — drives the URL the WASM uses for API calls

Because they are compile-time, the published `kronos-dashboard` Docker image is locked
to whatever values were set on the GitHub Actions runner (currently sourced from repo
`vars`). Operators cannot deploy the same image under a different prefix or point it at
a different API URL without rebuilding.

The API server (`TE_PATH_PREFIX`) is already runtime-configurable; this spec covers
only the dashboard.

## Goal

Ship one `kronos-dashboard` image whose path prefix and API base URL are set per
deployment via container environment variables, with no rebuild required.

## Approach: envsubst on `index.html` at container start

The image entrypoint runs `envsubst` over a templated `index.html` at container start,
producing a final `index.html` with `<meta>` tags carrying the runtime values. The
WASM reads those meta tags on boot.

### Asset URL strategy

Trunk's `--public-url` rewrites asset URLs (`<link rel="modulepreload" href="...">`,
the WASM/JS filenames) into `index.html` at build time. We cannot defer that to
runtime cheaply, because the filenames are content-hashed.

Build with `--public-url ./` so asset URLs are emitted as relative paths. Then add a
`<base href="${TE_DASHBOARD_PATH_PREFIX}/">` tag at the top of `<head>`, templated at
container start. The browser resolves all relative URLs in the document against the
`<base>` href, so:

- env unset → `<base href="/">` → assets load from `/` (today's behavior)
- env `/dashboard` → `<base href="/dashboard/">` → assets always load from
  `/dashboard/`, regardless of whether the user hits `/dashboard` or `/dashboard/`

This eliminates the trailing-slash problem — operators do not need a redirect, and the
nginx config stays dumb.

Normalization: the entrypoint strips a trailing `/` from `TE_DASHBOARD_PATH_PREFIX`
before substitution, so `/dashboard` and `/dashboard/` both produce
`<base href="/dashboard/">`.

### Runtime config injection

`crates/dashboard/index.html` becomes a template containing:

```html
<head>
  <base href="${TE_DASHBOARD_PATH_PREFIX}/">
  <meta name="te-path-prefix"  content="${TE_DASHBOARD_PATH_PREFIX}">
  <meta name="te-api-base-url" content="${TE_API_BASE_URL}">
  ...
</head>
```

The `<base>` drives asset URL resolution. The `<meta>` tags drive app-level config
(router base, API URL). Two mechanisms because they have distinct consumers — the
browser's URL resolver vs. our Rust code — and decoupling them avoids edge cases like
"router base differs from asset prefix."

After `trunk build`, the produced `dist/index.html` is renamed to `index.html.template`
inside the image so nginx never serves the un-substituted file.

A new entrypoint script `docker/dashboard/docker-entrypoint.sh`:

```sh
#!/bin/sh
set -e
# Normalize: strip trailing '/'; "/" or "" both become ""
PREFIX="${TE_DASHBOARD_PATH_PREFIX:-}"
PREFIX="${PREFIX%/}"
[ "$PREFIX" = "" ] && PREFIX=""
export TE_DASHBOARD_PATH_PREFIX="$PREFIX"

envsubst '${TE_DASHBOARD_PATH_PREFIX} ${TE_API_BASE_URL}' \
  < /usr/share/nginx/html/index.html.template \
  > /usr/share/nginx/html/index.html
exec nginx -g 'daemon off;'
```

The single-quoted variable list scopes substitution to those two names — any literal
`${...}` patterns inside emitted JS are untouched.

Unset env vars become empty strings, which is the same as "no prefix" / "API on same
origin" — matching today's no-prefix default.

### WASM reads at boot

A small helper in the dashboard reads a meta tag by name via `web_sys`:

```rust
fn read_meta(name: &str) -> String {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.query_selector(&format!("meta[name=\"{name}\"]")).ok().flatten())
        .and_then(|el| el.get_attribute("content"))
        .unwrap_or_default()
}
```

Normalization (strip trailing `/`, treat bare `/` as empty) lives next to it so we do
not have to trust the operator's input format. Same rules as the API-side
normalization in `crates/common/src/config.rs`.

`crates/dashboard/src/app.rs`:

- Replace the `option_env!`-fed `PATH_PREFIX` constant and its compile-time validation
  block with a one-shot startup read.
- Convert the resulting `String` to `&'static str` via `Box::leak(s.into_boxed_str())`
  so it satisfies `<Router base=...>`. Done once at app entry; the leak is bounded.
- `prefixed()` keeps the same signature; its body now uses the leaked static.

`crates/dashboard/src/api/client.rs`:

- `base_url()` replaces `option_env!("TE_API_BASE_URL")` with `read_meta("te-api-base-url")`.
- The existing fallback (derive from `window.location` when no value is set) is
  preserved, so dev mode continues to work without any meta tag.

### Container & CI changes

`Dockerfile.dashboard`:

- Drop `ARG TE_DASHBOARD_PATH_PREFIX` and `ARG TE_API_BASE_URL` along with the `ENV`
  re-exports and the conditional `--public-url` switch.
- Always build with `trunk build --release --public-url ./`.
- After copying `dist/` into the nginx serve stage, `RUN mv
  /usr/share/nginx/html/index.html /usr/share/nginx/html/index.html.template` so the
  image's canonical name is unambiguous and the entrypoint always reads from
  `.template` and writes to `index.html`.
- Copy in `docker-entrypoint.sh` and set it as `ENTRYPOINT`.

`docker/dashboard/nginx.conf`:

- Unchanged in spirit — the existing SPA fallback (`try_files $uri $uri/ /index.html;`)
  is what we want. The `<base>` tag in `index.html` handles asset resolution, so the
  nginx config does not need to know about the prefix.

`.github/workflows/release.yml`:

- Drop `TE_DASHBOARD_PATH_PREFIX` and `TE_API_BASE_URL` build-args from the
  `build-and-push-dashboard` job. The image is now generic.

## Out of scope

- Changing how the API server reads `TE_PATH_PREFIX` — already runtime.
- Adding a dashboard service to `docker-compose.prod.yml`. (Doable separately; not
  required for this change.)
- Authentication / API key handling for the dashboard. The hardcoded `dev-api-key`
  in `api/client.rs` is unchanged here.

## Validation plan

1. Build the image with no env: `docker build -f Dockerfile.dashboard .`. Run with
   nothing set; visit `/`. Dashboard loads, meta tags are empty, API calls go to
   same-origin. (Matches today's no-prefix default.)
2. Run with `TE_DASHBOARD_PATH_PREFIX=/dashboard TE_API_BASE_URL=http://localhost:8080/kronos`.
   Visit `/dashboard/`. Assets load (relative URLs), router treats `/dashboard` as
   base, API calls hit `http://localhost:8080/kronos/...`.
3. Restart the container with different env values. No rebuild. Confirm the new prefix
   is reflected in routing and API calls.
4. Visit `/dashboard` (no trailing slash). Confirm assets still load (`<base>` handles
   it) and the app renders correctly.
5. Re-run today's no-prefix dev workflow (`just dashboard`) to confirm
   `option_env!`-removal didn't break the dev path.

## Risks

- **`Box::leak` for router base.** Acceptable: one-shot leak at startup, bounded size.
  Alternative would be plumbing a `String` through Leptos which is heavier.
- **`<base href>` interaction with the router.** Leptos's `<Router base=...>` uses the
  meta tag, not `<base href>`. The two values must agree (entrypoint substitutes both
  from the same env var, so they will). Worth a note in the README so future readers
  do not edit one without the other.
- **Build-arg removal in CI.** Anyone consuming `vars.TE_DASHBOARD_PATH_PREFIX` /
  `vars.TE_API_BASE_URL` in the workflow will see those vars become unused. Cleanup
  task, not a regression.
