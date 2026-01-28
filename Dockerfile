# HIP-3 Bot Dockerfile
# Multi-stage build for smaller image size

# Stage 1: Builder
FROM rust:1.85-bookworm AS builder

WORKDIR /build

# Install dependencies for OpenSSL
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Build release binary
RUN cargo build --release --bin hip3-bot

# Stage 2: Runtime
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /build/target/release/hip3-bot /app/hip3-bot

# Copy config files
COPY config ./config

# Create data directory
RUN mkdir -p /app/data/mainnet/signals

# Create non-root user
RUN useradd -m -u 1000 hip3 && \
    chown -R hip3:hip3 /app

USER hip3

# Health check (process running - using /proc since pgrep not in slim image)
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD test -f /proc/1/exe || exit 1

ENTRYPOINT ["/app/hip3-bot"]
CMD ["--config", "/app/config/mainnet-phaseA-all.toml"]
