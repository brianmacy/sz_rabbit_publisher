# Status

## Current State (2026-06-29)

**Branch**: `harden-deps-ci-distroless`  
**PR**: [#32](https://github.com/brianmacy/sz_rabbit_publisher/pull/32) — "Harden deps/CI and switch runtime to distroless" (open, CI monitoring in progress)  
**Other open PRs**: #27 (lapin bump), #28 (dtolnay/rust-toolchain SHA bump), #29 (actions/checkout bump), #31 (codecov-action bump)  
**Working tree**: clean apart from an untracked stray `nohup.out` (deliberately not committed)

## Dockerfile Change (prior session)

Migrated runtime stage from `debian:bookworm-slim` (hand-rolled) to `gcr.io/distroless/cc-debian13:nonroot`.  
Builder upgraded from `rust:1.85` to `rust:1.88 AS builder`.  
Rationale: rust:1.88 targets Debian trixie (glibc 2.41); cc-debian12 (glibc 2.36) lacks glibc >= 2.38 symbols → "GLIBC_2.38 not found" at startup. Matches the distroless pattern used by sibling Senzing Rust ports.

## Prep Work Done (2026-06-29) — COMMITTED IN PR #32

All items below are complete, verified, and committed on `harden-deps-ci-distroless` (PR #32). CI monitoring in progress.

### Security Advisories — CLEARED
`cargo update` lifted all 6 open RUSTSEC advisories:
- RUSTSEC-2026-0098, -0099, -0104 — rustls-webpki 0.103.10 → 0.103.13 ✓
- RUSTSEC-2026-0118, -0119 — hickory-proto 0.25.2 → 0.26.1 ✓
- RUSTSEC-2026-0190 — anyhow 1.0.102 → 1.0.103 ✓
- Stale deny.toml ignores (RUSTSEC-2025-0134, RUSTSEC-2026-0009) removed ✓
- Added CDLA-Permissive-2.0 to deny.toml license allowlist (new transitive dep `webpki-root-certs`) ✓
- `cargo audit`: 0 vulnerabilities ✓
- `cargo deny check`: advisories ok, bans ok, licenses ok, sources ok ✓

### GitHub Actions SHA-Pinned
All `uses:` references in ci.yml, security.yml, release.yml replaced with 40-hex SHAs + `# version` comments:
- `actions/checkout@v6` → `df4cb1c0...`
- `actions/cache@v5` → `caa29612...`
- `actions/upload-artifact@v7` → `043fb46d...`
- `codecov/codecov-action@v6` → `fb8b3582...`
- `actions/create-release@v1` → `0cb9c9b6...`
- `actions/upload-release-asset@v1` → `e8f9f06c...`
- `dtolnay/rust-toolchain@stable` → `29eef336...` (pinned 2026-06-29)
- `dtolnay/rust-toolchain@master` → `67ef31d5...` (pinned 2026-06-29)
grep for tag-only pins returns empty ✓

### Dependabot Cooldown
`cooldown: default-days: 21` added to both cargo and github-actions entries in `.github/dependabot.yml` ✓

### Integration Test Skip Gate
All 5 integration tests now call `require_broker!()` macro at startup. When no broker is reachable:
- Emits a loud `eprintln!` skip notice
- Returns `Ok(())` (not a failure)
`cargo test` with no local broker: 22 unit tests pass, 5 integration tests loudly skip ✓

## Known Issues

None outstanding — all previous items resolved.
