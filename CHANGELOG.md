# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Published to [crates.io](https://crates.io/crates/mdya): `cargo install mdya` now works alongside the prebuilt installers. This path builds from source and requires `protoc`.

## [0.3.2] - 2026-06-13

### Changed

- **Breaking (MCP):** Consolidated the three search tools (`search_fts`, `search_vector`, `search_hybrid`) into a single `search` tool with a `mode` parameter (`"fts"` / `"vector"` / `"hybrid"`, default `"hybrid"`). MCP clients calling an old tool name must switch to `search` with the matching `mode`.
- The default log level is now `warn` (was `info`), so dependency `INFO` chatter (e.g. lance's misleading `status="error"` dataset-load events) no longer appears on a clean run. Restore the old verbosity with `--log-level info` or `RUST_LOG=info`.
- User-facing status (the `init` / `collection add` success lines and the MCP HTTP daemon's listening URL) now prints as plain text on stderr instead of as a log event, so it stays visible at the default log level.
- **Breaking:** `mdya update-all` and `mdya vector use` print their completion summary to stderr instead of stdout; stdout is now empty for these commands (reserved for piped data).

### Removed

- **Breaking (MCP):** Removed the `get_status` MCP tool. Index status remains available through the `mdya status` CLI command.

### Fixed

- `mdya update-all` no longer corrupts its progress display (duplicated bars / accumulated spinner rows) when log lines are emitted mid-render, and no longer panics in busy non-interactive (non-TTY) sessions. The progress bar is now rendered independently of the logging layer.

## [0.3.1] - 2026-06-12

### Added

- Contributor-facing community health files (`CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `SUPPORT.md`, `CODEOWNERS`, issue / pull request templates).
- README badges (license, latest release, CI status).
- Dependabot weekly updates for Cargo and GitHub Actions dependencies.
- Per-PR cross-platform `cargo check` smoke gate (Linux (arm64), Windows (x86_64), macOS (arm64 / x86_64)).
- Weekly + per-PR `cargo audit` workflow against the RustSec advisory database.

### Changed

- Bumped `lancedb` 0.29 → 0.30, `lance-index` 6.0 → 7.0, and `lindera` 0.44.1 → 3.0.7 (forced by lance-tokenizer 7.0's transitive switch to lindera 3.0).
- The on-disk `~/.mdya/lance-models/lindera/ipadic/config.yml` now uses lindera 3.0's URI dictionary scheme (`segmenter.dictionary: embedded://ipadic`) in place of the old nested form (`segmenter.dictionary.kind: ipadic`). The file is regenerated atomically on the next `mdya init` / `mdya update-all` run; no user action is required.

### Security

- `.cargo/audit.toml` with four upstream `unmaintained` (non-CVE) RUSTSEC IDs accepted as WONTFIX; see file header and `SECURITY.md` for the policy.

## [0.3.0] - 2026-06-10

### Added

- Initial public release.
- Markdown ingest with heading-aware chunking (`mdya update-all`).
- PDF ingest alongside Markdown.
- BM25 full-text search (`mdya search fts`).
- On-device vector search (`mdya search vector`) backed by a local embedding model; no cloud LLM API is contacted.
- Hybrid reciprocal-rank-fusion search (`mdya search hybrid`).
- MCP server (`mdya mcp`) exposing `search_fts`, `search_vector`, and `search_hybrid` over stdio and streamable HTTP.
- Prebuilt binaries and `curl | sh` / `irm | iex` installers for macOS (Apple Silicon, Intel), Linux (x86_64, aarch64), and Windows (x86_64).

[Unreleased]: https://github.com/yoshihirosuzuki/mdya/compare/v0.3.1...HEAD
[0.3.1]: https://github.com/yoshihirosuzuki/mdya/releases/tag/v0.3.1
[0.3.0]: https://github.com/yoshihirosuzuki/mdya/releases/tag/v0.3.0
