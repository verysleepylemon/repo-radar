# ── Stage 1: Build ────────────────────────────────────────────────────────────
FROM rust:1.81-slim AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs
RUN cargo build --release && rm -rf src

# Build the real binary
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/repo-radar /usr/local/bin/repo-radar

# Non-root user for security
RUN useradd -m -u 1001 reporadar
USER reporadar

ENTRYPOINT ["repo-radar"]
CMD ["watch"]
