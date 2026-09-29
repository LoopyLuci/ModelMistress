# =============================================================================
# Prometheus Serve - Multi-stage Dockerfile
# Supports: CPU-only, CUDA, and ROCm (AMD) builds
# =============================================================================

# --- Stage 1: Builder ---
FROM rust:1.78-bookworm AS builder

WORKDIR /build

# Cache dependencies
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release 2>/dev/null || true
RUN rm -rf src

# Copy source and build
COPY src ./src
RUN touch src/main.rs && cargo build --release

# --- Stage 2: Runtime ---
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    tini \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN groupadd -r prometheus && useradd -r -g prometheus -m prometheus

WORKDIR /app

# Copy binary from builder
COPY --from=builder /build/target/release/prometheus-serve /app/prometheus-serve

# Copy default config
RUN mkdir -p /etc/prometheus-serve /cache /models /logs

# Default config file
RUN echo '[server]\n\
listen_addr = "0.0.0.0:8000"\n\
request_timeout_ms = 30000\n\
\n\
[router]\n\
default_model = "llama-3-8b"\n\
enable_canary = false\n\
cloud_burst_enabled = false\n\
\n\
[observability]\n\
enable_tracing = true\n\
enable_metrics = true\n\
metrics_port = 9090\n\
log_level = "info"\n\
log_format = "json"\n\
\n\
[security]\n\
enable_mtls = false\n' > /etc/prometheus-serve/config.toml

# Own directories
RUN chown -R prometheus:prometheus /app /cache /models /logs /etc/prometheus-serve

EXPOSE 8000 9090

USER prometheus

ENTRYPOINT ["tini", "--"]
CMD ["/app/prometheus-serve"]

# =============================================================================
# Stage: ROCm (AMD GPU) variant
# Build with: docker build --target rocm --build-arg ROCM_VERSION=6.1.2 .
# =============================================================================
FROM builder AS rocm-build

ARG ROCM_VERSION=6.1.2

# Install ROCm packages for compilation
RUN apt-get update && apt-get install -y --no-install-recommends \
    wget gnupg2 && \
    wget -qO - https://repo.radeon.com/rocm/rocm.gpg.key | gpg --dearmor > /etc/apt/trusted.gpg.d/rocm-keyring.gpg && \
    echo "deb [arch=amd64 signed-by=/etc/apt/trusted.gpg.d/rocm-keyring.gpg] https://repo.radeon.com/rocm/apt/${ROCM_VERSION} bookworm main" > /etc/apt/sources.list.d/rocm.list && \
    apt-get update && apt-get install -y --no-install-recommends \
    rocm-dev rocm-libs hip-runtime-amd && \
    rm -rf /var/lib/apt/lists/*

ENV PATH="/opt/rocm/bin:${PATH}"
ENV LD_LIBRARY_PATH="/opt/rocm/lib:${LD_LIBRARY_PATH}"

# Build with ROCm feature
RUN cargo build --release --features rocm

FROM debian:bookworm-slim AS rocm

ARG ROCM_VERSION=6.1.2

# Install ROCm runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl tini wget gnupg2 && \
    wget -qO - https://repo.radeon.com/rocm/rocm.gpg.key | gpg --dearmor > /etc/apt/trusted.gpg.d/rocm-keyring.gpg && \
    echo "deb [arch=amd64 signed-by=/etc/apt/trusted.gpg.d/rocm-keyring.gpg] https://repo.radeon.com/rocm/apt/${ROCM_VERSION} bookmont main" > /etc/apt/sources.list.d/rocm.list && \
    apt-get update && apt-get install -y --no-install-recommends \
    rocm-dev rocm-libs hip-runtime-amd && \
    rm -rf /var/lib/apt/lists/*

RUN groupadd -r prometheus && useradd -r -g prometheus -m prometheus
WORKDIR /app

COPY --from=rocm-build /build/target/release/prometheus-serve /app/prometheus-serve
RUN mkdir -p /etc/prometheus-serve /cache /models /logs

COPY <<EOF /etc/prometheus-serve/config.toml
[server]
listen_addr = "0.0.0.0:8000"
request_timeout_ms = 30000

[router]
default_model = "llama-3-8b"
enable_canary = false
cloud_burst_enabled = false

[observability]
enable_tracing = true
enable_metrics = true
metrics_port = 9090
log_level = "info"
log_format = "json"

[security]
enable_mtls = false
EOF

RUN chown -R prometheus:prometheus /app /cache /models /logs /etc/prometheus-serve

EXPOSE 8000 9090

# ROCm environment
ENV ROCM_PATH="/opt/rocm"
ENV HIP_VISIBLE_DEVICES="0"
ENV HSA_OVERRIDE_GFX_VERSION="11.0.0"
ENV LD_LIBRARY_PATH="/opt/rocm/lib:${LD_LIBRARY_PATH}"

# Expose GPU devices
VOLUME ["/dev/dri", "/dev/kfd"]

USER prometheus
ENTRYPOINT ["tini", "--"]
CMD ["/app/prometheus-serve"]
