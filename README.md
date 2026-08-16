# mdya

*English / [日本語](README.ja.md)*

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)
[![Latest release](https://img.shields.io/github/v/release/yoshihirosuzuki/mdya)](https://github.com/yoshihirosuzuki/mdya/releases/latest)
[![CI](https://github.com/yoshihirosuzuki/mdya/actions/workflows/ci.yml/badge.svg)](https://github.com/yoshihirosuzuki/mdya/actions/workflows/ci.yml)

A small, fast search primitive for your local Markdown collections. BM25 full-text search, on-device vector search, and a hybrid that fuses both — exposed through two entry points: a **CLI** and an **MCP server**.

## Overview

mdya is a **search primitive**. It indexes local Markdown, and returns search results over a CLI and an MCP server. That is all it does.

mdya is not a search agent and not an LLM query-rewriting front-end. It has no query expansion, no reranker, and no multi-stage agent pipeline. Workflows that want those things are expected to layer a separate tool on top of mdya via its CLI or MCP interface.

It is local-first by construction. There is no path to any cloud LLM API; inference runs entirely on-device; distribution is a single binary.

What it can do:

- **BM25 full-text search** — full-text search with Japanese morphological analysis (lindera/ipadic) support.
- **Vector search** — cosine similarity over on-device embeddings (default `cl-nagoya/ruri-v3-30m`, 256 dimensions).
- **Hybrid search** — fuses BM25 and vector results via Reciprocal Rank Fusion (RRF).
- **Markdown chunking** — splits along heading and code-fence boundaries.
- **MCP server** — `mdya mcp` (stdio / HTTP) exposes the search tools to MCP clients.

`.pdf` files go through the same ingest path as `.md`. They are converted to plain text at ingest time and then chunked and embedded with the same pipeline as Markdown.

## Supported platforms

- macOS (arm64 / x86_64)
- Linux (amd64 / arm64)
- Windows (x86_64)

## Install

```sh
# Linux / macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/yoshihirosuzuki/mdya/releases/latest/download/mdya-installer.sh | sh
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://github.com/yoshihirosuzuki/mdya/releases/latest/download/mdya-installer.ps1 | iex"
```

Or grab an archive from the [releases page](https://github.com/yoshihirosuzuki/mdya/releases) and put the `mdya` binary on your `PATH`.

Rust users can install from [crates.io](https://crates.io/crates/mdya) instead. This builds from source, so it needs the [build-from-source prerequisites](#build-from-source) below (notably `protoc`):

```sh
cargo install mdya
```

### Build from source

Prerequisites:

- Rust 1.95+ (pinned automatically by `rust-toolchain.toml` at the repo root)
- `just` (`cargo install just`)
- `protoc` (Protocol Buffers compiler, a build dependency of the `lance` storage library). On macOS `brew install protobuf`; on Linux `apt install -y protobuf-compiler`; on Windows `choco install protoc`.

```sh
git clone https://github.com/yoshihirosuzuki/mdya.git
cd mdya
cargo build --release        # produces ./target/release/mdya
# or install onto your PATH:
cargo install --path .       # installs mdya into ~/.cargo/bin
```

## Getting started

The shortest path is `init` → `collection add` → `update-all` → `search`.

```sh
mdya init                       # create ~/.mdya/ (config.yml + data directories for the index)
mdya collection add ~/notes     # register a directory as a collection (name = basename, here: notes)
mdya update-all                 # walk every registered collection, ingest .md / .markdown / .pdf, build the index
mdya search fts "release plan"     # BM25 full-text search
mdya search vector "release plan"  # vector search
mdya search hybrid "release plan"  # hybrid (RRF)
```

> The first `mdya update-all` (and vector / hybrid searches) automatically downloads the embedding model (`cl-nagoya/ruri-v3-30m`, about 140 MB) into `~/.mdya-models/`, and reuses that cache afterwards.

Search results are printed to stdout in a human-readable format.

```
notes/release.md  score=0.812
  > The release procedure is as follows
  > 1. Update the version number
---
notes/checklist.md  score=0.751
  > Pre-release checklist
---
2 doc hits (showing 20 max)
```

Common flags:

- `-c, --collections <NAMES...>` — restrict to specific collections (default: all)
- `-n, --limit <N>` — top N hits (default 20)
- `--format <FORMAT>` — output format (default `human`). `json` (machine-friendly envelope), `md` (Markdown, suitable for pasting or LLM input), and `xml` (structurally equivalent to JSON, also for LLM input) are also available.

## MCP server

mdya can run as an MCP (Model Context Protocol) server, so clients like Claude Code or Claude Desktop can call its search tools.

```sh
mdya mcp        # start the MCP server over stdio
```

Registering with Claude Code:

```sh
claude mcp add mdya -- mdya mcp
```

## Further reading

For configuration, individual subcommands, and MCP details, see the [user manual](docs/manual/en/README.md).

## Configuration

Configuration lives in a single file at `~/.mdya/config.yml` (a template is generated by `mdya init`). Minimal example:

```yaml
collections:
  notes:
    path: ~/notes
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime:
  memory_limit_mb: 8192     # upper bound on this process's RSS, in MB. 0 to disable.
  embed_parallelism: 8      # number of files embedded in parallel during update-all. 0 for sequential.
```

`mdya collection add <path>` rewrites the `collections` section, so you usually do not need to edit it by hand. The configuration directory can be moved with `--config-dir <path>`, and the embedding model cache with `--model-cache-dir <path>` (default `$HOME/.mdya-models/`).

## Development

```sh
just check    # fmt-check + clippy (-D warnings) + cargo test (full workspace) + cargo audit
just smoke    # smoke tests only
just          # list available recipes
```

`just check` needs `cargo install cargo-audit --locked` and network access — the advisory database is fetched on every run. Offline, run `cargo audit --no-fetch --stale` instead.

## Contributing

Contributions are welcome on a best-effort basis (one-person maintainer; no formal SLA). Please read the following before opening an issue or PR:

- [CONTRIBUTING.md](CONTRIBUTING.md) — issue / PR workflow, build and test setup.
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) — Contributor Covenant.
- [SECURITY.md](SECURITY.md) — private channels for security vulnerabilities.
- [SUPPORT.md](SUPPORT.md) — where to ask questions.

## License

Dual-licensed under MIT OR Apache-2.0. See [`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE).
