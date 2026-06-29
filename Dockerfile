# Multi-stage build for minimal image size
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

# Runtime stage
# cc-debian13 (NOT cc-debian12): the builder is rust:1.88 on Debian trixie (glibc 2.41),
# so the release binary may reference glibc >= 2.38 symbols that cc-debian12 (glibc 2.36)
# lacks -> "GLIBC_2.38 not found" at startup. The sibling Senzing ports hit exactly this
# and standardized on cc-debian13. It ships CA certificates (/etc/ssl/certs/ca-certificates.crt,
# needed by lapin's rustls TLS) and a built-in nonroot user (uid 65532); :nonroot runs as it.
FROM gcr.io/distroless/cc-debian13:nonroot

# Copy binary from builder
COPY --from=builder /usr/src/sz_rabbit_publisher/target/release/sz_rabbit_publisher /usr/local/bin/

# Set entrypoint
ENTRYPOINT ["sz_rabbit_publisher"]
CMD ["--help"]
