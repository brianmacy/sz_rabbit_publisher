# Multi-stage build for minimal image size
# Builder pinned to the crate MSRV (edition 2024 / rust-version 1.88).
FROM rust:1.88 AS builder

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

# Runtime stage — distroless (no shell, no package manager, runs as nonroot).
# cc-debian12 provides glibc + libgcc (Rust gnu target) and bundles
# ca-certificates for AMQPS/TLS. Decompression is pure-Rust (lbzip2 + flate2),
# so no system bzip2/zlib libraries are needed at runtime.
FROM gcr.io/distroless/cc-debian12:nonroot

# Copy binary from builder
COPY --from=builder /usr/src/sz_rabbit_publisher/target/release/sz_rabbit_publisher /usr/local/bin/

# Set entrypoint (absolute path; distroless has no shell for PATH fallback)
ENTRYPOINT ["/usr/local/bin/sz_rabbit_publisher"]
CMD ["--help"]
