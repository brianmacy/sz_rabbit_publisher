# Multi-stage build for minimal image size
FROM rust:1.85 as builder

WORKDIR /usr/src/sz_rabbit_publisher

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Create dummy main to cache dependencies
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy source code
COPY src ./src

# Build the application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -u 1000 appuser

# Copy binary from builder
COPY --from=builder /usr/src/sz_rabbit_publisher/target/release/sz_rabbit_publisher /usr/local/bin/

# Switch to non-root user
USER appuser

# Set entrypoint
ENTRYPOINT ["sz_rabbit_publisher"]
CMD ["--help"]
