# ── Stage 0: Build dashboard WASM (skipped when INCLUDE_DASHBOARD != true) ──
FROM rust:bookworm AS dashboard-builder

ARG INCLUDE_DASHBOARD="false"

RUN if [ "$INCLUDE_DASHBOARD" = "true" ]; then \
      apt-get update && apt-get install -y pkg-config libssl-dev cmake && rm -rf /var/lib/apt/lists/* \
      && rustup target add wasm32-unknown-unknown \
      && cargo install wasm-pack --locked \
      && ARCH=$(dpkg --print-architecture) \
      && if [ "$ARCH" = "arm64" ]; then TW_ARCH="linux-arm64"; else TW_ARCH="linux-x64"; fi \
      && curl -sL "https://github.com/tailwindlabs/tailwindcss/releases/download/v3.4.17/tailwindcss-${TW_ARCH}" \
           -o /usr/local/bin/tailwindcss \
      && chmod +x /usr/local/bin/tailwindcss; \
    fi

WORKDIR /app
COPY . .

RUN if [ "$INCLUDE_DASHBOARD" = "true" ]; then \
      cd crates/dashboard && wasm-pack build --target web --release -- --features hydrate \
      && tailwindcss -i input.css -o pkg/tailwind-output.css --minify; \
    else \
      mkdir -p crates/dashboard/pkg; \
    fi

# ── Stage 1: Plan dependencies (cargo-chef) ─────────────────────────────
FROM lukemathwalker/cargo-chef:latest-rust-bookworm AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 2: Build API/worker binary ────────────────────────────────────
FROM chef AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

COPY --from=planner /app/recipe.json recipe.json

ARG FEATURES=""

# Cache dependency build (rebuilds only when Cargo.toml/lock change). The git
# cache matters: git dependencies (oci_kms lives in the ~470 MB hyperswitch
# repo) are otherwise re-cloned on every build.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked,id=cargo-registry \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked,id=cargo-git \
    --mount=type=cache,target=/app/target,sharing=locked,id=cargo-target \
    if [ -n "$FEATURES" ]; then \
      cargo chef cook --release --recipe-path recipe.json --features "$FEATURES"; \
    else \
      cargo chef cook --release --recipe-path recipe.json; \
    fi

COPY . .

ARG BINARY

RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked,id=cargo-registry \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked,id=cargo-git \
    --mount=type=cache,target=/app/target,sharing=locked,id=cargo-target \
    if [ -n "$FEATURES" ]; then \
      cargo build --release --bin "$BINARY" --features "$FEATURES"; \
    else \
      cargo build --release --bin "$BINARY"; \
    fi \
    && cp /app/target/release/"$BINARY" /usr/local/bin/app

# ── Stage 3: Runtime ────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/app /usr/local/bin/app
COPY --from=dashboard-builder /app/crates/dashboard/pkg /app/dashboard-dist

ENV INVOKR_DASHBOARD_DIST_DIR=/app/dashboard-dist

ENTRYPOINT ["/usr/local/bin/app"]
