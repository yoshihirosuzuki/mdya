default:
    @just --list

# CI runs the first three per pull request (`ci.yml`). The advisory scan is a
# scheduled monitor there (`audit.yml`), not a pull-request gate, so a local
# run is the only per-change signal for it.

# The cargo-audit check runs first even though the scan itself runs last: it
# is the only step that fails on a missing tool rather than on the code, and
# learning that after a cold-cache `cargo test --workspace` costs half an hour.

# Format check + lint + test + advisory scan.
check: _require-cargo-audit fmt-check lint test audit

# Verify formatting.
fmt-check:
    cargo fmt --all --check

# Apply formatting.
fmt:
    cargo fmt --all

# Run clippy in workspace, warnings are errors. This also checks the stdout,
# environment variable, and panic rules in Cargo.toml `[lints.clippy]` and
# clippy.toml.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run the full test suite.
test:
    cargo test --workspace

# Most integration tests under tests/ are ignored by default, each one that
# runs the real embedding model among them, and neither `check` nor CI runs
# them. `--test '*'` selects the integration tests only, and leaves out the
# ignored tests and doc examples in src/: some of those fetch up to 1.2 GB for
# each model or need Ollama, and are run by hand as their modules describe.
# The first run fetches the default embedding model, about 150 MB, into
# target/.

# Run the integration tests that are ignored by default.
integration:
    cargo test --test '*' -- --ignored

# The advisory database is fetched over the network on every run: a stale copy
# would miss exactly the advisories published since the last run. Offline, run
# `cargo audit --no-fetch --stale` by hand — `--no-fetch` alone still errors
# once the local database is 90 days old. This recipe is part of `check` even
# though it needs the network: the scan takes seconds, and a dependency bump
# that pulls in a vulnerable crate should not have to wait for the nightly job
# to surface.

# Scan deps against RustSec; needs cargo-audit and network access.
audit: _require-cargo-audit
    cargo audit --deny warnings

_require-cargo-audit:
    @command -v cargo-audit >/dev/null || { echo "cargo-audit not found; install it with: cargo install cargo-audit --locked"; exit 1; }

# Print the dependency tree with features enabled, for native-dep auditing.
inspect-deps:
    cargo tree -e features
