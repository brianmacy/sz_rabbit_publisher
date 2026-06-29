# Next Steps

## Immediate (ready to commit)

1. **Commit the prep work** — all five fix areas are complete and verified:
   - `Cargo.lock` — dependency updates (security fixes)
   - `deny.toml` — removed stale ignores, added CDLA-Permissive-2.0
   - `.github/workflows/ci.yml`, `security.yml`, `release.yml` — SHA-pinned actions
   - `.github/dependabot.yml` — cooldown added
   - `tests/integration_test.rs` — broker skip gate
   - `CHANGELOG.md` — [Unreleased] entries for all changes
   - `STATUS.md`, `NEXT_STEPS.md` — updated

   Suggested commit sequence:
   ```
   git add Dockerfile CHANGELOG.md STATUS.md NEXT_STEPS.md
   git commit -m "chore: migrate runtime to distroless/cc-debian13:nonroot"

   git add Cargo.lock deny.toml .github/dependabot.yml .github/workflows/ci.yml \
           .github/workflows/security.yml .github/workflows/release.yml \
           tests/integration_test.rs
   git commit -m "chore: prep — security, SHA-pinned actions, dependabot cooldown, test skip gate"
   ```

## Near-term

2. **Merge or close open Dependabot PRs** — the cargo update in this session likely supersedes:
   - PR #27: lapin 4.10.0 (already on 4.10.0 now)
   - PR #28: dtolnay/rust-toolchain SHA bump (now SHA-pinned manually)
   - PR #29: actions/checkout 6.0.3 (now SHA-pinned)
   - PR #31: codecov-action 7.0.0 (kept at v6 SHA)

3. **Re-pin dtolnay/rust-toolchain** periodically — `@master` and `@stable` are rolling
   branch heads, not fixed releases. The SHA pinned here (2026-06-29) will drift.
   Re-resolve quarterly or when the toolchain needs updating.

## Ongoing

4. **Monitor transitive dependency advisories** — all current advisories are cleared but
   the lapin → tcp-stream → async-rs dep chain has historically been slow to update.
   Run `cargo audit` and `cargo deny check` before each release.
