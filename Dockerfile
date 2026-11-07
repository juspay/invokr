# ── Stage 0: Build dashboard WASM (skipped when INCLUDE_DASHBOARD != true) ──
FROM rust:1.94.1-bookworm AS dashboard-builder

ARG INCLUDE_DASHBOARD="false"

RUN if [ "$INCLUDE_DASHBOARD" = "true" ]; then \
      apt-get update && apt-get install -y pkg-config libssl-dev cmake && rm -rf /var/lib/apt/lists/* \
      && rustup target add wasm32-unknown-unknown \
      && cargo install wasm-pack --version 0.13.1 --locked \
      && ARCH=$(dpkg --print-architecture) \
      && if [ "$ARCH" = "arm64" ]; then \
           TW_ARCH="linux-arm64"; TW_SHA256="69b1378b8133192d7d2feb12a116fa12d035594f58db3eff215879e4ad8cf39b"; \
         else \
           TW_ARCH="linux-x64"; TW_SHA256="7d24f7fa191d2193b78cd5f5a42a6093e14409521908529f42d80b11fde1f1d4"; \
         fi \
      && curl -fsSL "https://github.com/tailwindlabs/tailwindcss/releases/download/v3.4.17/tailwindcss-${TW_ARCH}" \
           -o /usr/local/bin/tailwindcss \
      && echo "${TW_SHA256}  /usr/local/bin/tailwindcss" | sha256sum -c - \
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
FROM lukemathwalker/cargo-chef:0.1.77-rust-1.94.1-bookworm AS chef
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
