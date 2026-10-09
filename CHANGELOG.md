# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.2] - 2026-10-09

### Added

- `--transient` (env `RABBITMQ_TRANSIENT`): publish `delivery_mode=1` instead of
  the default persistent `2` (#37). Affects classic queues only — quorum queues
  persist every message regardless of delivery mode. Transient messages in a
  classic queue are lost on broker restart. Confirms, mandatory returns and
  retries are unchanged.

### Changed

- Dependencies (one rolled-up change for Dependabot #54, #52, #51, #50, #47,
  #45, #41, #39, #35): hickory-resolver 0.26.3, lapin 4.11.0, flate2 1.1.10,
  clap 4.6.6, tokio 1.53.1, anyhow 1.0.104, crossbeam-epoch 0.9.20
  (RUSTSEC-2026-0204); CI actions/checkout v7.0.1, actions/cache v6.1.0, and
  every `dtolnay/rust-toolchain` use pinned to one master SHA with an explicit
  `toolchain:` input.

### Removed

- The release workflow's crates.io publish job. This crate is not published to
  crates.io; the job always failed for lack of a token and marked every release
  run failed.

## [0.6.1] - 2026-10-09

### Fixed

- `--version` reported a hardcoded `0.1.0`. Release builds now report the
  release tag (set at build time from the tag via `RELEASE_VERSION`; the
  release workflow fails if `--version` does not match the tag). Other builds
  report the `Cargo.toml` version, now kept in step (0.6.1).

### Changed

- Release binaries are published for Linux only: x86_64 and aarch64, each as
  glibc and fully static musl (aarch64 built natively on GitHub arm runners).
  The macOS and Windows builds were removed from the release workflow and from
  the v0.6.0 release. The release workflow can be re-run for an existing tag
  (`workflow_dispatch`, `tag` input) and uploads with `--clobber`. macOS (aarch64) returns once Developer ID signing and
  notarization are configured.

## [0.6.0] - 2026-10-09

### Fixed — "never drop a record"

Each item has a real-broker test in `tests/backpressure_test.rs` that fails on
the previous release.

- Unroutable messages were acked by the broker and silently discarded while
  counted as `acked` (`mandatory=false`). Publishes are now `mandatory`; a
  returned message is counted as `returned`, logged as an ERROR and retried
  until a binding exists.
- A confirm failure dropped the rest of the in-flight batch (`break` out of
  `pending.drain(..)` discarded it): records were lost on a connection kill and
  the run still exited 0. Every unconfirmed message is now re-published.
- Reconnect could hang forever when the broker was mid-restart (a connect
  attempt never completed). Connect and close attempts are now bounded (15s).
- Multi-member gzip input (`cat a.gz b.gz`, pigz, bgzip) stopped silently after
  the first member. Now decoded with `MultiGzDecoder`.
- A non-UTF-8 line, or any read error, ended the read with only an ERROR log and
  exit 0, dropping the rest of the file. Lines are now read as bytes (non-UTF-8
  lines are published verbatim); a genuine read/decompression error fails the
  run non-zero with the exact `--skip-lines` resume point.
- The run now fails non-zero unless `acked == total` records read.
- `--skip-lines` docs claimed over-skipping was safe; it loses every record
  never sent. Docs corrected and a warning is logged when skipping.
- `pending` no longer stays above 0 after a connection failure.
- An unroutable record could still be counted as acked and lost when a confirm
  batch mixed routable and unroutable publishes: lapin 4.10 attaches a
  `basic.return` to an arbitrary tag of a coalesced `basic.ack multiple=true`
  (`complete_pending_before` iterates a `HashMap`), so the really-returned
  message surfaced as a plain ack. Any batch with a return now re-publishes all
  of its acked messages (possible duplicates, counted `republished`).
- `--skip-lines` resume hint on a stop signal in multi-file mode now says to
  resume the file on its own (`--skip-lines` is rejected with several files).
- Records ending in several `\r`s before `\n` keep stripping ALL trailing `\r`
  (byte-reader regression restored to the pre-change behaviour).

### Security

- `rustls` 0.23.41 -> 0.23.45 (RUSTSEC-2026-0285); yanked `chacha20` 0.10.1 ->
  0.10.2 and `spin` 0.9.8 -> 0.9.9. MSRV stays 1.88. `cargo deny` advisories
  clean; unused license allowances dropped.

### Added

- `returned` and `republished` (possible duplicates) stats; nacks are logged.
- `connection.blocked` / `connection.unblocked` (resource alarms) are logged,
  plus a periodic warning while a confirm is withheld.
- SIGINT/SIGTERM print the summary and the exact resume point (`--skip-lines`)
  and exit 130/143.
- `tests/backpressure_test.rs` (Docker, `--ignored`; `rabbitmq:4.3-management`
  and quorum queues by default) and a CI job running it on RabbitMQ 4.3 for
  quorum and classic queues.

### Changed

- "Not acknowledged" in the summary is renamed "Nack retries" (it counts every
  retry, not distinct records).

## [0.5.0] - 2026-07-22

### Added

- `--skip-lines N` / `SENZING_SKIP_LINES`: skip the first N non-empty records
  before publishing, to resume an interrupted single-file load. Compressed inputs
  (gzip/bzip2) have no seek, so the skipped prefix is decoded and discarded. Rejected
  loudly with more than one input file (per-file skipping would silently drop records).
  (Corrected in Unreleased: over-skipping LOSES records; only under-skipping is safe.)

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
