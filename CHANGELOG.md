# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
