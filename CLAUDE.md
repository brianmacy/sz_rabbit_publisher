# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

High-performance RabbitMQ publisher written in Rust for publishing JSONL (JSON Lines) files to RabbitMQ queues.

## Language & Standards

- **Rust Edition 2024**
- Clippy must pass with: `--all-targets --all-features -- -D warnings`
- Use real implementations, no mocks in tests
- 100% test coverage required

## Build & Test Commands

```bash
# Build
cargo build --release

# Run tests
cargo test

# Run single test
cargo test test_name

# Clippy (must pass with no warnings)
cargo clippy --all-targets --all-features -- -D warnings

# Format code
cargo fmt

# Check formatting
cargo fmt -- --check
```

## Architecture

The project publishes JSONL files to RabbitMQ with focus on:

- High throughput and low latency
- Efficient file parsing of JSONL format
- Reliable message delivery to RabbitMQ
- CLI-first configuration (command-line arguments and environment variables)

## Configuration

- Configuration via CLI arguments with environment variable support
- Do not hardcode connection strings, queue names, or other parameters
- Environment variables for sensitive values (RABBITMQ_URL, RABBITMQ_EXCHANGE, etc.)
- Sensible defaults provided for all optional parameters
- Priority: CLI arguments > environment variables > defaults

## Testing Requirements

- `test_*` functions must fail on any error
- `example_*` functions must succeed completely
- Use real RabbitMQ connections in integration tests (no mocks)
- Smoke tests required for end-to-end validation
- Memory safety must be verified (no segfaults, heap corruption)

## Dependencies

Key dependencies:

- RabbitMQ client library (lapin)
- Async runtime (tokio)
- CLI parsing (clap with derive and env features)
- Gzip support (flate2)
- Error handling (anyhow)
- Logging (tracing, tracing-subscriber)
