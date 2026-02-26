# Security Policy

## Supported Versions

We currently support the following versions with security updates:

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

We take security vulnerabilities seriously. If you discover a security vulnerability in sz_rabbit_publisher, please report it responsibly.

### How to Report

1. **Do not** open a public GitHub issue for security vulnerabilities
2. Email the maintainer directly with details of the vulnerability
3. Include as much information as possible:
   - Type of vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

### What to Expect

- **Acknowledgment**: We'll acknowledge receipt within 48 hours
- **Initial Assessment**: We'll provide an initial assessment within 7 days
- **Fix Timeline**: We'll work on a fix and keep you updated on progress
- **Disclosure**: Once a fix is ready, we'll coordinate disclosure timing with you
- **Credit**: You'll be credited in the security advisory (unless you prefer anonymity)

## Security Best Practices

When using sz_rabbit_publisher:

### Credentials Management

- **Never hardcode credentials** in code or configuration files
- Use **environment variables** for sensitive configuration:
  ```bash
  export RABBITMQ_URL="amqp://user:password@host:5672/%2F"
  ```
- Consider using secret management systems (HashiCorp Vault, AWS Secrets Manager, etc.)
- Restrict file permissions on any files containing credentials

### Network Security

- Use **TLS/SSL** for RabbitMQ connections in production:
  ```bash
  sz_rabbit_publisher -u "amqps://user:password@host:5671/%2F" data.jsonl
  ```
- Configure firewall rules to restrict access to RabbitMQ ports
- Use VPNs or private networks when possible

### Input Validation

- Validate JSONL file contents before publishing
- Sanitize any user-provided input
- Be cautious with files from untrusted sources

### Container Security

When using Docker:

- Run container as non-root user (already configured in Dockerfile)
- Keep base images updated
- Scan images for vulnerabilities regularly
- Use minimal base images (debian-slim in this case)

### Monitoring

- Monitor for unusual publishing patterns
- Set up alerts for failed authentications
- Track message delivery failures
- Log security-relevant events

## Known Security Considerations

### Dependency Management

- We regularly audit dependencies using `cargo audit`
- Security workflows run daily to check for vulnerabilities
- Dependencies are kept up-to-date

#### Known Advisories (Transitive Dependencies)

The following advisories exist in transitive dependencies and are tracked for resolution:

**RUSTSEC-2025-0134: rustls-pemfile unmaintained**

- **Path:** lapin → amq-protocol-tcp → tcp-stream → rustls-pemfile
- **Severity:** Maintenance (non-critical)
- **Impact:** Low - library is unmaintained but functional
- **Mitigation:** Not directly exploitable; waiting for tcp-stream to migrate to rustls-pki-types
- **Status:** Monitoring upstream for fixes

**RUSTSEC-2026-0009: time crate RFC 2822 DoS**

- **Path:** lapin → amq-protocol-tcp → tcp-stream → p12-keystore → x509-parser → time 0.3.45
- **Severity:** Medium (6.8)
- **Impact:** DoS via stack exhaustion with malicious RFC 2822 input
- **Mitigation:** Our code path does not parse RFC 2822 formatted data
- **Status:** Requires time >= 0.3.47; waiting for upstream x509-parser/p12-keystore updates

**Action Plan:**

- Monitor lapin, tcp-stream, and related crates for updates
- Upgrade dependencies when fixes are available
- Re-run `cargo audit` after each dependency update
- See `deny.toml` for detailed tracking information

### Memory Safety

- Rust's memory safety guarantees protect against common vulnerabilities
- No unsafe code blocks in the codebase
- All dependencies are vetted

### Error Handling

- Errors are logged but sensitive information is never exposed
- Failed operations don't leak credentials or internal state
- Proper cleanup on errors to prevent resource leaks

## Security Updates

Security updates will be released as:

- **Patch versions** (0.1.x) for minor security issues
- **Minor versions** (0.x.0) for moderate security issues
- **Major versions** (x.0.0) for breaking changes required for security

Subscribe to GitHub releases to be notified of security updates.

## Security Features

### Built-in Security Features

- **No credential logging**: Sensitive data never logged
- **Secure defaults**: Conservative default settings
- **Input validation**: File paths and parameters validated
- **Graceful degradation**: Fails safely on errors
- **Resource limits**: Back pressure prevents resource exhaustion

### Compile-Time Protections

- Memory safety via Rust
- Type safety prevents common bugs
- No buffer overflows
- No use-after-free
- No data races

## Compliance

This software:

- Does not collect or store user data
- Does not phone home
- Does not include telemetry
- Processes data locally only

## Third-Party Dependencies

We maintain a security-focused approach to dependencies:

- Regular audits with `cargo audit`
- Minimal dependency tree
- Well-maintained, reputable crates only
- License compliance (MIT/Apache-2.0)

## Questions?

For security-related questions that don't involve vulnerabilities, open a GitHub issue labeled "security question".
