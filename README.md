# mdya

*English / [日本語](README.ja.md)*

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#license)
[![Latest release](https://img.shields.io/github/v/release/yoshihirosuzuki/mdya)](https://github.com/yoshihirosuzuki/mdya/releases/latest)
[![CI](https://github.com/yoshihirosuzuki/mdya/actions/workflows/ci.yml/badge.svg)](https://github.com/yoshihirosuzuki/mdya/actions/workflows/ci.yml)

A tool for searching the Markdown and PDF documents on your own computer, from the command line or from an AI assistant over MCP.

## Overview

- You can use full-text search, vector search (which finds text by closeness in meaning), and hybrid search, which combines the two.
- The documents to search are ingested from directories, archives (zip, tar, and gzip-compressed tar), and document content passed in with a command.
- There are two entry points: the CLI commands, and the MCP server, which you use from an AI assistant (such as Claude Code).
- It does not use cloud LLM APIs. Embeddings (sequences of numbers that represent the meaning of text, used for vector search) are made on your computer. There is also a setting that leaves making the embeddings to Ollama.
- It runs as a single executable.

mdya covers the work up to returning search results. It has no models for rewriting queries or for reordering results.

### Document language

The default settings are meant for Japanese documents. The default embedding model, `cl-nagoya/ruri-v3-30m`, is a Japanese model, and full-text search splits text into words with a Japanese dictionary (IPADIC).

For documents in languages other than Japanese, you can choose embedding models that may suit them: `google/embeddinggemma-300m`, `sentence-transformers/all-MiniLM-L6-v2` (English), and Ollama models. However, search accuracy has not been verified for documents in languages other than Japanese ([Settings: Document language](docs/manual/en/configuration.md#document-language)).

### If you used an earlier version

mdya has been rebuilt. The rebuilt mdya is not compatible with mdya v0.4.3 and earlier.

## Supported operating systems

mdya runs on macOS, Linux, and Windows. The releases page has prebuilt binaries for the following:

- macOS (arm64, x86_64)
- Linux (x86_64, arm64)
- Windows (x86_64)

## Installation

```sh
# Linux and macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/yoshihirosuzuki/mdya/releases/latest/download/mdya-installer.sh | sh
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://github.com/yoshihirosuzuki/mdya/releases/latest/download/mdya-installer.ps1 | iex"
```

Or, download a prebuilt binary from the [releases page](https://github.com/yoshihirosuzuki/mdya/releases), and put `mdya` in a directory on your `PATH`.

You can also install from [crates.io](https://crates.io/crates/mdya). This builds from source, so you need the prerequisites in "Building from source" below.

```sh
cargo install mdya
```

### Building from source

Prerequisites:

- Rust 1.95 or later (the repository's `rust-toolchain.toml` sets the version)
- `protoc` (the Protocol Buffers compiler, used to build dependency crates). On macOS, `brew install protobuf`; on Linux, `apt install -y protobuf-compiler`; on Windows, `choco install protoc`
- A network connection. During the build, the dependency crates and the Japanese dictionary for full-text search (from `https://Lindera.dev/`) are downloaded. When building without a network connection, see "Building without a network connection" below.

```sh
git clone https://github.com/yoshihirosuzuki/mdya.git
cd mdya
cargo build --release        # builds ./target/release/mdya
# Or, to put it on your PATH:
cargo install --path .       # installs mdya into ~/.cargo/bin
```

#### Building without a network connection

While you have a network connection, download what the build downloads (the dependency crates and the dictionary for full-text search) to the computer you build on. Install Rust and `protoc` beforehand as well.

1. In the repository directory, download the dependency crates. After `cargo fetch`, Cargo can run without a network connection as long as `Cargo.lock` does not change.

   ```sh
   cargo fetch --locked
   ```

2. Download the dictionary tarball, and put it in `3.0.7/` under the directory for dictionaries (here, `$HOME/lindera`). `3.0.7` is the version of the `lindera-ipadic` crate that mdya uses, and you can check it in `Cargo.lock`.

   ```sh
   mkdir -p "$HOME/lindera/3.0.7"
   curl -sSfL -o "$HOME/lindera/3.0.7/mecab-ipadic-2.7.0-20250920.tar.gz" https://Lindera.dev/mecab-ipadic-2.7.0-20250920.tar.gz
   ```

3. Without a network connection, set the environment variable `LINDERA_DICTIONARIES_PATH` to the directory for dictionaries, and build with `--frozen`. With `--frozen`, Cargo does not use the network, and when it tries to change `Cargo.lock`, it stops with an error.

   ```sh
   LINDERA_DICTIONARIES_PATH="$HOME/lindera" cargo build --release --frozen
   ```

## Getting started

These are the steps from ingesting the Markdown in a directory to searching it.

```sh
mdya collection add notes ~/notes   # make ~/notes the collection "notes"
mdya update notes                   # ingest the .md, .markdown, and .pdf files in ~/notes
mdya search "リリースの手順"           # search
mdya get notes release.md           # read a document that was found
```

The first time you ingest (and the first time you run a vector search or hybrid search), mdya downloads the embedding model (`cl-nagoya/ruri-v3-30m`, about 150 MB) from Hugging Face and puts it in `~/.mdya-models`. From the second time on, it reads the model from there.

Search results look like this:

```
$ mdya search "リリースの手順"
1. notes  meetings/2026-11-10.md
   score 0.0331, chunk 2 of 3
   ## 次にやること - リリースの手順を見直す。 - 会議室の予約を、来月の分まで取る。 

2. notes  release.md
   score 0.0331, chunk 0 of 3
   # リリースの手順 タグを打つ前に、変更履歴を書き、バージョン番号を上げます。 

3. notes  setup.md
   score 0.0323, chunk 0 of 1
   # 開発環境の用意 新しい PC で開発を始めるときの手順です。 1. リポジトリを取得します。 2. 必要な道具を…
```

Each hit shows the collection name and key, the score and chunk index, and a snippet (the text around where the match was found).

After you change files, running `mdya update notes` re-ingests only the documents that changed. To re-ingest all collections, run `mdya update --all`.

You can also do the following:

- Make an archive (`.zip`, `.tar`, `.tar.gz`, `.tgz`) a collection: `mdya collection add <name> <archive> --type archive`
- Pass document content with a command: create a collection with `mdya collection add <name> --type raw`, and pass the content with `mdya put`
- Choose the search mode: `mdya search --mode fts` (full-text search), `--mode vector` (vector search), `--mode hybrid` (the default)
- Get results as JSON: `--json`

## Using mdya from an AI assistant (MCP)

`mdya mcp` runs as an MCP server. Register it with Claude Code as follows:

```sh
claude mcp add --transport stdio mdya -- mdya mcp
```

With the MCP tools, an AI assistant can list collections (`list_collections`), search (`search`), and get documents (`get_document`). The MCP tools are read-only. Create collections and ingest documents with the CLI, as in "Getting started" above.

## Manual

Detailed usage is in the [manual](docs/manual/en/README.md).

- [Collections and collection types](docs/manual/en/collections.md)
- [Commands](docs/manual/en/commands.md)
- [Settings](docs/manual/en/configuration.md)
- [The MCP server](docs/manual/en/mcp.md)
- [Errors and how to fix them](docs/manual/en/errors.md)

## Development

```sh
just check    # format check, clippy (warnings are errors), cargo test (the whole workspace), cargo audit
just          # list the recipes
```

In addition to the prerequisites in "Building from source" and `just`, `just check` needs `cargo-audit` (`cargo install cargo-audit --locked`) and a network connection (it downloads the advisory database every time). When you have no network connection, run `cargo audit --no-fetch --stale` instead.

## Contributing

Issues and PRs are handled on a best-effort basis (there is one maintainer, and no response deadlines are promised). Before opening an issue or PR, please read the following:

- [CONTRIBUTING.md](CONTRIBUTING.md): the flow for issues and PRs, and the steps to build and test.
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md): Contributor Covenant.
- [SECURITY.md](SECURITY.md): how to report a security vulnerability without making it public.
- [SUPPORT.md](SUPPORT.md): where to ask questions.

## License

Dual-licensed under MIT OR Apache-2.0. See [`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE).
