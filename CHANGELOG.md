# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
- Dual license (MIT OR Apache-2.0)

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
