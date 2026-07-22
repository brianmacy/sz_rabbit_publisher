# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2026-07-22

### Added

- `--skip-lines N` / `SENZING_SKIP_LINES`: skip the first N non-empty records
  before publishing, to resume an interrupted single-file load. Compressed inputs
  (gzip/bzip2) have no seek, so the skipped prefix is decoded and discarded. Rejected
  loudly with more than one input file (per-file skipping would silently drop records).
  Safe by idempotency: the engine's `add_record` treats a re-published record as an
  update, so over-skipping never loses data and under-skipping only re-sends a few.

### Security

- Bumped dependency versions to clear all open RUSTSEC advisories via `cargo update`:
  - `rustls-webpki` 0.103.10 → 0.103.13 (clears RUSTSEC-2026-0098, -0099, -0104)
  - `anyhow` 1.0.102 → 1.0.103 (clears RUSTSEC-2026-0190 unsoundness in `Error::downcast_mut`)
  - `hickory-proto` 0.25.2 → 0.26.1 (clears RUSTSEC-2026-0118, -0119)
  - Also removed stale deny.toml advisory ignores (RUSTSEC-2025-0134, RUSTSEC-2026-0009 now fixed)
  - Added `CDLA-Permissive-2.0` to deny.toml license allowlist (new transitive dep `webpki-root-certs` via `rustls-platform-verifier`)
- SHA-pinned all GitHub Actions in ci.yml, security.yml, and release.yml; every
  `uses:` line now references a 40-hex commit SHA with a `# vX.Y.Z` or
  `# <branch> (pinned YYYY-MM-DD)` comment

### Changed

- Added `cooldown: default-days: 21` to every ecosystem entry in `.github/dependabot.yml`
  to dampen noisy update PRs

### Fixed

- Integration tests now check broker reachability at startup and emit a loud
  `eprintln!` skip notice when no RabbitMQ broker is available (e.g., local dev
  with no broker running). `cargo test` without a broker now passes unit tests
  and loudly skips integration tests rather than failing with ACCESS-REFUSED.

### Added

- Multi-file support: accept multiple JSONL files as positional arguments
- `--parallel` / `-p` flag to publish all files concurrently (one AMQP connection per file)
- Overall summary printed when processing multiple files
- `--help` now documents progress output fields
- bzip2 (`.bz2`) input support, auto-detected by magic bytes (`BZh`) alongside gzip.
  Uses the `bzip2` crate's `MultiBzDecoder` (pure-Rust `libbz2-rs-sys` backend), so
  concatenated streams (e.g. `pbzip2`/`lbzip2` output) decode fully. Decode is
  single-threaded per file; concurrency across files comes from `--parallel`

### Changed

- Upgraded Docker builder from `rust:1.85` to `rust:1.88` (Debian trixie / glibc 2.41)
  to match the crate MSRV (edition 2024 / rust-version 1.88)
- Replaced `debian:bookworm-slim` runtime stage with distroless `gcr.io/distroless/cc-debian13:nonroot`
  (no shell or package manager, runs as nonroot); cc-debian13 (not cc-debian12) is
  required because rust:1.88 may reference glibc >= 2.38 symbols absent in cc-debian12 (glibc 2.36),
  and distroless ships CA certificates needed by lapin's rustls TLS plus a built-in nonroot user
- Replaced sequential per-message publisher confirms with pipelined batch confirms
  - Each `PublisherConfirm` is awaited individually to verify actual broker ack/nack
  - Nacked messages (reject-publish) are retried forever with configurable delay
  - Eliminates one broker RTT (~5ms) per message, targeting ~18k msg/s (up from ~170 msg/s)
- Added automatic reconnection: connection drops (e.g., PostgreSQL reboot) trigger
  infinite retry with no message loss — unconfirmed messages are re-published after reconnect
- Progress reporting now fires on acked milestones; rate shows interval throughput, not cumulative average
- Removed `publish_with_retry` method (replaced by batch pipeline with reconnection)

### Planned

- Upgrade dependencies when upstream fixes security advisories (see SECURITY.md)

## [0.1.0] - 2025-02-07

### Added

- Initial release of sz_rabbit_publisher
- High-performance async RabbitMQ publisher for JSONL files
- Automatic gzip file detection and decompression (magic byte detection)
- Publisher confirms with automatic retry on nack (up to 3 attempts)
- Back pressure mechanism using bounded channels (tokio mpsc)
- CLI-first interface with environment variable support
- Progress reporting at configurable intervals (default: every 10,000 messages)
- Comprehensive test suite (15 unit tests, 5 integration tests)
- GitHub Actions CI/CD workflows (CI, Release, Security)
- Docker-based integration tests using testcontainers
- Multi-platform support (Linux, macOS, Windows)
- Dockerfile for containerized deployments
- Comprehensive documentation (README, CONTRIBUTING, SECURITY, CHANGELOG)
- Apache-2.0 license

### Features

- Publishes JSONL files to RabbitMQ queues
- Supports both plain text and gzip-compressed files
- Delivery confirmations (ack/nack) with automatic retry logic
- Flow control to prevent overwhelming RabbitMQ
- Configurable max pending messages (default: 500)
- Persistent messages (delivery_mode=2)
- Thread-safe statistics tracking with real-time reporting
- Graceful shutdown on completion or Ctrl+C
- Environment variable support for sensitive credentials
- Verbose logging mode for troubleshooting

### Configuration

- CLI arguments for all options (--url, --exchange, --queue, --routing-key, etc.)
- Environment variables (RABBITMQ_URL, RABBITMQ_EXCHANGE, RABBITMQ_QUEUE, RABBITMQ_ROUTING_KEY)
- Sensible defaults for all parameters
- Priority: CLI args > env vars > defaults

### Performance

- Expected 2-5x faster than Python implementation
- Lower memory usage with efficient async I/O
- Natural flow control via bounded channels
- Minimal dependency footprint

### Dependencies

- tokio (async runtime)
- lapin (RabbitMQ AMQP client)
- clap (CLI parsing with derive and env features)
- flate2 (gzip support)
- anyhow (error handling)
- tracing & tracing-subscriber (logging)

### Testing

- 15 unit tests (all passing)
- 5 integration tests with real RabbitMQ
- No mock implementations (real implementations only)
- Clippy passes with -D warnings
- Code formatted with rustfmt
