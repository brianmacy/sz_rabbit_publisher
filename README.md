# sz_rabbit_publisher

High-performance RabbitMQ publisher written in Rust for publishing JSONL (JSON Lines) files to RabbitMQ queues.

## Features

- **High Performance**: Async I/O with tokio for maximum throughput
- **Back Pressure**: Bounded channel implementation prevents memory exhaustion
- **Delivery Confirmations**: Publisher confirms with automatic retry on nack
- **Gzip Support**: Automatically detects and decompresses gzip files
- **Progress Reporting**: Configurable progress updates during publishing
- **CLI-First**: All configuration via command-line arguments or environment variables
- **Type Safety**: Compile-time guarantees with Rust's type system

## Installation

### Build from Source

```bash
cargo build --release
```

The binary will be available at `target/release/sz_rabbit_publisher`.

## Usage

### Basic Usage

```bash
sz_rabbit_publisher \
  -u "amqp://guest:guest@localhost:5672/%2F" \
  -e "my-exchange" \
  -q "my-queue" \
  -r "my.routing.key" \
  data.jsonl
```

### Using Environment Variables (Recommended for Production)

For security, use environment variables for sensitive configuration:

```bash
export RABBITMQ_URL="amqp://user:password@host:5672/%2F"
export RABBITMQ_EXCHANGE="my-exchange"
export RABBITMQ_QUEUE="my-queue"
export RABBITMQ_ROUTING_KEY="my.routing.key"

sz_rabbit_publisher data.jsonl
```

### With Gzip-Compressed Files

The tool automatically detects gzip compression:

```bash
sz_rabbit_publisher data.jsonl.gz
```

### Custom Throttling

Control the maximum number of pending confirmations:

```bash
sz_rabbit_publisher -m 1000 large_file.jsonl
```

### Verbose Logging

Enable detailed logging for troubleshooting:

```bash
sz_rabbit_publisher -v data.jsonl
```

## Command-Line Options

```
sz_rabbit_publisher [OPTIONS] <INPUT_FILE>

Arguments:
  <INPUT_FILE>  Path to JSONL file (plain text or gzip)

Options:
  -u, --url <AMQP_URL>           RabbitMQ connection URL
                                  [env: RABBITMQ_URL]
                                  [default: amqp://guest:guest@localhost:5672/%2F]
  -e, --exchange <EXCHANGE>      Exchange name
                                  [env: RABBITMQ_EXCHANGE]
                                  [default: senzing-rabbitmq-exchange]
  -q, --queue <QUEUE>            Queue name
                                  [env: RABBITMQ_QUEUE]
                                  [default: senzing-rabbitmq-queue]
  -r, --routing-key <KEY>        Routing key
                                  [env: RABBITMQ_ROUTING_KEY]
                                  [default: senzing.records]
  -m, --max-pending <NUM>        Max pending confirmations
                                  [default: 500]
  --report-interval <NUM>        Progress report interval (messages)
                                  [default: 10000]
  --retry-delay <SECS>           Retry delay on nack (seconds)
                                  [default: 3]
  -v, --verbose                  Enable verbose logging
  -h, --help                     Print help
  -V, --version                  Print version
```

## Configuration Priority

Configuration values are resolved in the following order (highest to lowest priority):

1. Command-line arguments
2. Environment variables
3. Default values

Example:

```bash
# Environment variable sets URL
export RABBITMQ_URL="amqp://env-host:5672/%2F"

# CLI argument overrides environment variable
sz_rabbit_publisher -u "amqp://cli-host:5672/%2F" data.jsonl
# Uses: amqp://cli-host:5672/%2F
```

## Architecture

### Back Pressure Mechanism

The publisher implements proper back pressure to prevent overwhelming RabbitMQ:

1. **Bounded Channel**: File reader sends lines to a bounded channel (capacity = max_pending)
2. **Automatic Blocking**: When channel is full, file reader blocks automatically
3. **Natural Flow Control**: System self-regulates based on RabbitMQ's processing capacity
4. **Memory Safety**: Prevents memory exhaustion on large files

### Delivery Guarantees

- **Publisher Confirms**: Enabled by default for delivery guarantees
- **Automatic Retry**: Messages that receive nack are automatically retried (up to 3 attempts)
- **Persistent Messages**: All messages published with delivery_mode=2 (persistent)
- **Graceful Shutdown**: Waits for all confirmations before exiting

## Development

### Running Tests

#### Unit Tests

```bash
cargo test --lib
```

All unit tests run without external dependencies.

#### Integration Tests

Integration tests require RabbitMQ to be running. Use Docker Compose for easy setup:

```bash
# Start RabbitMQ with pre-configured test infrastructure
docker-compose -f docker-compose.test.yml up -d

# Wait for RabbitMQ to be ready
sleep 5

# Run integration tests
cargo test --test integration_test

# Stop RabbitMQ when done
docker-compose -f docker-compose.test.yml down
```

The Docker Compose setup automatically creates all necessary exchanges, queues, and bindings defined in `test-config/rabbitmq-definitions.json`.

### Code Quality

```bash
# Run clippy
cargo clippy --all-targets --all-features -- -D warnings

# Format code
cargo fmt

# Check formatting
cargo fmt -- --check
```

## Performance

### Comparison to Python Version

Expected improvements over the Python implementation:

- **2-5x faster throughput**: Async I/O and zero-copy optimizations
- **Lower memory usage**: No intermediate buffering
- **Better back pressure**: Natural flow control via bounded channels
- **Type safety**: Compile-time guarantees prevent runtime errors

### Benchmarking

To benchmark performance:

```bash
# Build release version
cargo build --release

# Time execution
time ./target/release/sz_rabbit_publisher large_file.jsonl
```

## Troubleshooting

### Connection Issues

```bash
# Test connection with verbose logging
sz_rabbit_publisher -v test.jsonl

# Check RabbitMQ is accessible
curl http://localhost:15672/api/overview
```

### Performance Tuning

```bash
# Increase max pending for higher throughput (uses more memory)
sz_rabbit_publisher -m 2000 data.jsonl

# Decrease max pending for lower memory usage (slower throughput)
sz_rabbit_publisher -m 100 data.jsonl
```

### File Format

Ensure your JSONL file has one JSON object per line:

```jsonl
{"id": 1, "name": "Alice"}
{"id": 2, "name": "Bob"}
{"id": 3, "name": "Charlie"}
```

Empty lines are automatically filtered out.

## Requirements

- Rust 1.85 or later (Edition 2024)
- RabbitMQ server (tested with 3.x)
- Docker (for integration tests only)

## License

MIT OR Apache-2.0

## Contributing

Contributions welcome! Please ensure:

1. All tests pass: `cargo test`
2. Clippy passes: `cargo clippy --all-targets --all-features -- -D warnings`
3. Code is formatted: `cargo fmt`
4. New features include tests
