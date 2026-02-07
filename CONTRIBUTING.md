# Contributing to sz_rabbit_publisher

Thank you for your interest in contributing to sz_rabbit_publisher! This document provides guidelines for contributing to the project.

## Code of Conduct

This project adheres to a code of conduct. By participating, you are expected to uphold this code. Please be respectful and constructive in all interactions.

## How to Contribute

### Reporting Bugs

Before creating bug reports, please check existing issues to avoid duplicates. When creating a bug report, include:

- Clear and descriptive title
- Steps to reproduce the issue
- Expected behavior
- Actual behavior
- Environment details (OS, Rust version, RabbitMQ version)
- Relevant logs or error messages

### Suggesting Enhancements

Enhancement suggestions are tracked as GitHub issues. When creating an enhancement suggestion, include:

- Clear and descriptive title
- Detailed description of the proposed functionality
- Examples of how the enhancement would be used
- Any potential drawbacks or considerations

### Pull Requests

1. **Fork the repository** and create your branch from `main`
2. **Make your changes** following the coding standards
3. **Add tests** for any new functionality
4. **Ensure all tests pass** (`cargo test`)
5. **Run clippy** and fix all warnings (`cargo clippy --all-targets --all-features -- -D warnings`)
6. **Format your code** (`cargo fmt`)
7. **Update documentation** as needed
8. **Write a clear commit message** describing your changes
9. **Submit your pull request** with a comprehensive description

## Development Setup

### Prerequisites

- Rust 1.85 or later
- Docker (for integration tests)
- RabbitMQ server (for manual testing)

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release
```

### Running Tests

```bash
# Unit tests
cargo test --lib

# Integration tests (requires Docker)
cargo test --test integration_test

# All tests
cargo test
```

### Code Quality

```bash
# Run clippy
cargo clippy --all-targets --all-features -- -D warnings

# Format code
cargo fmt

# Check formatting
cargo fmt -- --check
```

## Coding Standards

### Rust Style

- Follow Rust edition 2024 conventions
- Use `rustfmt` for formatting (enforced in CI)
- Pass `clippy` with `-D warnings` (enforced in CI)
- Write idiomatic Rust code
- Prefer `match` over `if/else` chains for enums

### Testing

- **100% test coverage required** for new code
- **No mock tests** - use real implementations
- Test functions must start with `test_*`
- Example functions must start with `example_*`
- Integration tests must use real RabbitMQ instances
- Tests must fail on any error (no ignored errors)

### Documentation

- Public APIs must have documentation comments (`///`)
- Use clear, concise language
- Include examples where appropriate
- Update README.md for user-facing changes
- Keep CLAUDE.md updated for AI-assisted development guidance

### Commit Messages

- Use clear and meaningful commit messages
- Start with a verb in imperative mood (e.g., "Add", "Fix", "Update")
- Keep the first line under 72 characters
- Provide additional context in the body if needed

Example:

```
Add retry logic for transient RabbitMQ errors

Implements exponential backoff for connection failures and
channel errors. Retries up to 3 times with configurable delays.

Fixes #123
```

## Project Structure

```
sz_rabbit_publisher/
├── .github/
│   └── workflows/       # GitHub Actions workflows
├── src/
│   ├── main.rs         # CLI entry point
│   ├── lib.rs          # Library exports
│   ├── publisher.rs    # Core publisher logic
│   ├── file_reader.rs  # File reading with gzip support
│   └── stats.rs        # Statistics tracking
├── tests/
│   └── integration_test.rs  # Integration tests
├── Cargo.toml          # Project manifest
├── README.md           # User documentation
├── CLAUDE.md           # AI development guidance
└── CONTRIBUTING.md     # This file
```

## Release Process

1. Update version in `Cargo.toml`
2. Update `CHANGELOG.md` with release notes
3. Create a git tag: `git tag -a v0.1.0 -m "Release v0.1.0"`
4. Push tag: `git push origin v0.1.0`
5. GitHub Actions will automatically build and create a release

## Getting Help

- Open an issue for bugs or feature requests
- Check existing issues and pull requests
- Review the README.md for usage documentation
- Read the code documentation (`cargo doc --open`)

## License

By contributing to sz_rabbit_publisher, you agree that your contributions will be licensed under the same license as the project (MIT OR Apache-2.0).
