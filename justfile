default:
    @just --list

# Format check + lint + test, the gate used by CI.
check: fmt-check lint test

# Verify formatting.
fmt-check:
    cargo fmt --all --check

# Apply formatting.
fmt:
    cargo fmt --all

# Run clippy in workspace, warnings are errors.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run the full test suite.
test:
    cargo test --workspace

# Run only the integration smoke tests.
smoke:
    cargo test --workspace --test smoke_cli --test smoke_init --test smoke_lancedb --test smoke_candle --test smoke_modernbert --test smoke_mcp_stdio

# Regenerate every committed smoke fixture (writes tests/fixtures/{tiny_bert,tiny_modernbert}/*).
gen-fixtures:
    cargo run -p xtask-generate-tiny-bert
    cargo run -p xtask-generate-tiny-modernbert

# Production embedding e2e (network required): downloads cl-nagoya/ruri-v3-30m
# into a temp cache and runs the real forward pass. Run manually; not part of
# `just check`.
embed-e2e:
    cargo test --test e2e_embed_real -- --ignored --nocapture

# Full `mdya update-all` end-to-end (network required, ~140 MB model
# download on first run). Mirrors `embed-e2e` for the ingest path; not
# part of `just check`.
update-all-e2e:
    cargo test --test e2e_update_all -- --ignored --nocapture

# Full CLI golden-path E2E (network required, ~140 MB model download on
# first run): init -> collection add -> update-all -> search 3 mode ->
# mcp, through the real binary. Not part of `just check`.
full-flow-e2e:
    cargo test --test e2e_full_flow -- --ignored --nocapture

# Empirical LanceDB index behaviour verification.
# Runs every `#[ignore]` axis in `verify_lancedb_indexes.rs` once and
# prints findings via `[finding] ...` stdout lines. Not part of
# `just check` — the suite hits the real `lancedb` engine and
# benchmarks at 10K rows. `--test-threads=1` is required to keep
# axis 08 / 09 / 12 latency samples stable; parallel runs contend on
# CPU and skew the medians.
lancedb-verify:
    cargo test --test verify_lancedb_indexes -- --ignored --nocapture --test-threads=1

# Empirical verification of lindera/ipadic placement prerequisites.
# Runs each `#[ignore]` axis and prints findings via `[ipadic finding]` lines.
# Not part of `just check` — the suite hits the real `lancedb` engine.
# Tests are isolated via tempdir + `LANCE_LANGUAGE_MODEL_HOME` override,
# so the user's `<system data dir>/lance/...` tree is never touched.
# `--test-threads=1` is required because the override mutates process
# global env state.
lindera-ipadic-verify:
    cargo test --test verify_lindera_ipadic -- --ignored --nocapture --test-threads=1

# Empirical verification that the schema metadata pin (used in place of a
# separate `embedding_model` column) survives Arrow Schema persistence under
# LanceDB / Lance. Findings print as `[metadata finding]` lines.
schema-metadata-verify:
    cargo test --test verify_schema_metadata -- --ignored --nocapture --test-threads=1

# Print the dependency tree with features enabled, for native-dep auditing.
inspect-deps:
    cargo tree -e features
