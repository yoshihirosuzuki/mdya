# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- Bumped `rustls` 0.23.45 → 0.23.46 in the lockfile; only the `rustls` entry's `version` and `checksum` moved, and no manifest change was needed. This clears the four advisories that rustls 0.23.46 fixes:
  - GHSA-c8m5-c952-6cm4 / RUSTSEC-2026-0346 (moderate): formatting a client session value (`Tls12ClientSessionValue`, `Tls13ClientSessionValue`) with `Debug` printed its TLS 1.2 master secret or TLS 1.3 resumption key.
  - GHSA-vp88-798v-h67f / RUSTSEC-2026-0347 (low): when a server rejected Encrypted Client Hello (ECH), application data the client had buffered before the handshake completed was still sent to the outer server.
  - GHSA-h2v8-394m-p7vv / RUSTSEC-2026-0348 (low): the optional Brotli certificate decompressor could be made to allocate up to 1 GiB before the peer was authenticated.
  - GHSA-w44r-93j7-qv28 / RUSTSEC-2026-0349 (low): an unauthenticated `ServerHello` could leave later connections to the same server name offering only a classical X25519 share, so they could complete without the post-quantum hybrid key exchange.

  In the binary, `rustls` reaches mdya only through `reqwest` 0.12, which `hf-hub` uses to download the embedding model, `lancedb`'s `lance-namespace-reqwest-client` uses, and mdya uses for the Ollama backend. As far as `cargo tree` shows, `ring` is the only crypto provider compiled into `rustls` there, and the `brotli` feature is off. Only seven crates in the lockfile depend on `rustls`, on `hyper-rustls`, or on a crate that re-exports `rustls` (`tokio-rustls`, `quinn`, `quinn-proto`), and so can name `rustls` types. Their sources and mdya's were checked, and no path to any of the four was found in the binary:
  - GHSA-c8m5-c952-6cm4: none of them names the session value types or implements a session store, and the advisory notes that the built-in session cache keeps the values redacted.
  - GHSA-vp88-798v-h67f: none of them configures ECH.
  - GHSA-h2v8-394m-p7vv: the Brotli decompressor is not compiled in.
  - GHSA-w44r-93j7-qv28: the `ring` provider's key exchange groups (X25519, secp256r1, secp384r1) include no post-quantum hybrid to lose.

  Building from source also runs `rustls` as a client, through `reqwest` 0.13 with the `aws-lc-rs` provider, when the `lindera-ipadic` build script downloads the IPADIC dictionary. The reasons above for GHSA-c8m5-c952-6cm4, GHSA-vp88-798v-h67f and GHSA-h2v8-394m-p7vv hold there too: `brotli` is off in that build as well, and `reqwest` 0.13 is one of the seven crates checked. For GHSA-w44r-93j7-qv28, the `prefer-post-quantum` feature is off, so that provider lists the X25519MLKEM768 hybrid last, after X25519. The client opens each connection with the key exchange group it remembers for the server name, or with X25519 when it remembers none. Judging by the `rustls` sources, an attacker can make it remember any of the classical groups (X25519, secp256r1, secp384r1), but the client then opens without a hybrid share, as it does with nothing remembered, and a server that wants the hybrid answers with a HelloRetryRequest, which the advisory lists as not triggering the flaw.

- Bumped `rustls-webpki` 0.103.15 → 0.103.16 in the lockfile; only its `version` and `checksum` moved. This clears RUSTSEC-2026-0382 (CVE-2026-111943): certificate verification could accept a path through distrusted emSign intermediate certificates, whose private keys are now widely known, when the trust anchors allowed it. 0.103.16 rejects any path containing those intermediates' public keys, whichever root store supplied the anchor. Judging by the sources, and without having tried it, the flaw is likely reachable in the binary's default configuration:
  - When the embedding model is not cached yet, mdya downloads it from `https://huggingface.co` through `hf-hub` and `reqwest` 0.12.
  - `reqwest` 0.12 trusts both the bundled `webpki-roots`, which still carried `emSign ECC Root CA - G3`, and the operating system's roots.
  - Neither `hf-hub` nor mdya checks the contents of the downloaded files, so an attacker in the network path holding one of those keys could hand mdya a substituted model.

  Building from source on Linux also uses `rustls-webpki`, with the operating system's roots, when the `lindera-ipadic` build script downloads the IPADIC dictionary through `rustls-platform-verifier` under `reqwest` 0.13. That script also compares the download's MD5 with a fixed value.

- Bumped `webpki-roots` 1.0.7 → 1.0.11 in the lockfile; only its `version` and `checksum` moved. This clears RUSTSEC-2026-0381 (the same CVE-2026-111943): the bundled root store included `emSign ECC Root CA - G3`, which 1.0.10 removes. `reqwest` 0.12 adds these roots to its clients in the binary, alongside the operating system's. The releases in between also add and remove other roots, so a server whose chain ends at a removed root may no longer be reachable unless the operating system's roots also accept it:
  - 1.0.8 removes SecureSign Root CA12.
  - 1.0.9 adds two Telia roots (EC and RSA TLS Root CA v3) and two SECOM roots (TLS ECC and TLS RSA Root CA 2024).
  - 1.0.9 also removes Atos TrustedRoot 2011.
  - 1.0.11 removes emSign Root CA - C1 and emSign ECC Root CA - C3.

- Bumped `webpki-root-certs` 1.0.7 → 1.0.11 in the lockfile; only its `version` and `checksum` moved. This clears RUSTSEC-2026-0383 (the same CVE-2026-111943), the same root removal as in `webpki-roots`, for the list of the roots' full certificates. In the lockfile only `rustls-platform-verifier` depends on it, and only when building for wasm32, so no build of mdya for its release targets compiles it.

## [0.4.2] - 2026-10-11

### Added

- Chunk range reads: `mdya get --chunk <N> --chunk-end <M>` and the MCP `get_document` `chunk_end` parameter return chunks `N` through `M` (inclusive) as one contiguous piece of the original text, so a caller can read around a search hit in one call, or page through a document too large to fetch whole in pieces that fit the size cap. An end past the last chunk reads to the end of the document; `--chunk-end` without `--chunk`, or an end smaller than the start, is an error (MCP code `invalid_chunk_range`).
- Every search hit, at doc and chunk level, now carries `chunk_count`, the number of chunks in its document — in the JSON / XML / Markdown output (`chunk_count`), the human output (`chunks=N`), and the MCP `search` result.
- MCP error code `index_outdated`, returned by `search` and by `get_document` with `chunk` when the index needs rebuilding (see below).
- `mdya update-all` and `mdya vector use` now report progress where the progress bars are hidden — when stderr is not a terminal (redirected to a file, piped, a CI log), or when `TERM` is `dumb` (or, outside Windows, unset) — and nothing used to appear until the final summary. The state is printed as one line when indexing starts and then every 10 seconds: the collection being processed, its files done out of its total, and each file in progress with the chunks embedded so far, for example `Indexing 'notes': 12/340 files; in progress: docs/big.pdf (320/12000 chunks), memo.md`. While the search indexes are updated at the end, the line reads `Updating search indexes`.
- With the progress bars, the line of each file in progress now shows the chunks embedded so far out of the file's total (`docs/big.pdf 320/12000`), so a single large document visibly moves, and `Updating search indexes` is shown while the search indexes are updated at the end.
- Archive collections: `mdya collection add` now also accepts a `.zip` / `.tar` / `.tar.gz` / `.tgz` file, and `mdya update-all` indexes the `.md` / `.markdown` / `.pdf` documents inside it without extracting it. An archive has no default collection name, so `--name` is required. Each document is indexed under its path inside the archive, and changes are detected from the archive's own modification time. Symbolic and hard links do not become documents; the file a link points to is indexed under its own name when it is in the archive. GNU tar and bsdtar sparse files are rebuilt and read. Documents that cannot be read — a name that is not UTF-8, is absolute, or contains `..` or a control character; a zip entry that is encrypted or compressed with anything other than deflate or no compression; more than 256 MiB decompressed; a sparse file whose layout cannot be used — count as `failed`. An archive that cannot be read at all — missing, corrupt or truncated; a `.tar` / `.tar.gz` / `.tgz` that does not end with its end-of-archive marker; a zip in which the compressed data of several entries overlaps; document names that add up to more than 256 MiB, counting 128 bytes more for each; a zip split across several files, or with other data in front of it whose internal offsets were not adjusted — makes `mdya update-all` and `mdya vector use` exit with `1`, and the other collections are still updated. `mdya update-all` leaves that collection's index as it was; after `mdya vector use`, its documents drop out of search until `mdya update-all` reads the archive again. An archive whose modification time or size changes while it is read is not indexed from that point on: while its contents are being listed, it cannot be read at all; while its documents are being read, those not read yet count as `failed` and keep their previous index entries. The next `mdya update-all` reads it again.

### Changed

- **Breaking:** `mdya get --chunk <N>` and the MCP `get_document` `chunk` parameter now return the part of the original document the chunk covers — heading markers, link targets, HTML and all — instead of the chunk's plain-text body. Chunk ranges cover the document without gaps, so a read of chunks `0` through the last returns the whole document.
- **Breaking:** existing indexes must be rebuilt. The index now records where each chunk sits in its document, which an index built by an earlier version lacks; `mdya update-all`, `mdya search`, `mdya get --chunk` and the MCP `search` / chunk reads stop with an error naming the command to run, `mdya vector use <current model>`. `mdya vector use` now rebuilds such an index even when the model is unchanged. If `mdya mcp` is running, restart it after the rebuild. Full-document reads keep working on an old index.
- The `get.cli_max_bytes` / `get.mcp_max_bytes` caps now apply to chunk reads too, since a chunk range can span a whole document. `-f` / `--no-size-limit` bypasses the CLI cap as before.
- **Breaking (library):** `mdya::get::GetError::DocumentTooLarge` is renamed `ContentTooLarge`; `mdya::chunking::Chunk` gains `source_range`; both `mdya::search::SearchHit` variants gain `chunk_count`; `mdya::mcp::GetDocumentRequest` gains `chunk_end`; `GetError`, `mdya::search::SearchError`, `mdya::ingest::IngestError` and `mdya::mcp::McpErrorCode` gain variants. `mdya::ingest::UpdateSummary` gains `unreadable_collections` and is no longer `Copy`; `mdya::cli::vector::VectorUseError` gains `CollectionsUnreadable`; `mdya::config::InvalidPathKind::NotDirectory` is renamed `NotDirectoryOrArchive`, and the enum gains `ArchiveNeedsName`. In `mdya::ingest::IngestProgress`, `set_total_files` is replaced by `start_collection`, which also takes the collection name, and the trait gains the required methods `chunks_embedded` and `start_index_maintenance`.
- `mdya update-all` (and the re-embed walk in `mdya vector use`) now honors `.gitignore`. When a collection root is inside a git repository, files and directories excluded by `.gitignore` — including rules in `.gitignore` files above the root — are no longer indexed; git worktrees, `node_modules/` and scratch directories kept under a repository root used to be indexed and appeared as duplicates in search results. Only `.gitignore` is read (not `.git/info/exclude`, the global excludes file or `.ignore`), and hidden directories are not excluded by name. Existing indexes adjust on the next `mdya update-all` or `mdya vector use`: entries for paths that are now ignored are dropped and counted under `removed`.
- `mdya update-all` (and the re-embed walk in `mdya vector use`) no longer indexes a file whose path contains a control character (Unicode category Cc, such as a newline, a tab or ESC). Printed in a log line, the progress display or a search result, such a path could move the cursor or recolor the terminal, and XML cannot represent most of the control characters below U+0020 at all. The file is not read and counts as `failed`, so `mdya update-all` and `mdya vector use` exit with `1` while it is there, and the warning shows its path with the control characters escaped. An index entry for such a path is dropped on the next `mdya update-all` or `mdya vector use` and counted under `removed`. Warnings now escape control characters in error messages too, since a message from code whose wording mdya does not control (PDF extraction, decompression, the file system) may quote text from the file it is about.
- `tar` and `flate2` are now direct dependencies, for archive collections, and `zip` is a dev-dependency for their tests. All three were already in the dependency tree, so no crate is added. `tar` and its dependency `filetime`, used only at build time until now, are now part of the mdya binary.
- Replaced the direct `walkdir` dependency with the `ignore` crate (the library behind ripgrep); walkdir stays in the tree underneath it. `ignore` and `globset` are the only crates new to the dependency tree, both pure Rust with no `*-sys` bindings.
- `pulldown-cmark` is now pinned to `=0.13.4` (was `0.13`). The Markdown chunker's workaround for a pulldown-cmark offset-iterator panic ([pulldown-cmark/pulldown-cmark#1129](https://github.com/pulldown-cmark/pulldown-cmark/issues/1129)) is verified only against 0.13.4. `cargo install mdya` without `--locked` could otherwise pick up a later 0.13.x, on which the workaround could miss an input and abort `mdya update-all` or `mdya vector use`. Crates that depend on the `mdya` library are held to 0.13.4 as well.

### Removed

- **Breaking (library):** `mdya::get::get_chunk`, replaced by `mdya::get::get_chunks`, which takes a `ChunkSpan`.

### Fixed

- Items of a list written without blank lines between them were joined into one run of text in the indexed chunk (`- alpha` / `- beta` became `alphabeta`), and so were nested list items and list items inside block quotes. Every block boundary now separates the text, whatever the Markdown layout. This takes effect when the index is rebuilt.
- Some Markdown documents were indexed only up to a certain list item, and the rest of the document was silently left out of search. Such documents are rare: for example, one with a list item holding only a link reference definition (`- [a]: https://example.com`), directly followed by a whitespace-only line indented at least four columns past the item's text (six or more spaces under a top-level `- ` item). They are now indexed in full, chunked as plain text (Markdown syntax and any YAML front matter included in the indexed text) instead of by headings. This takes effect when the index is rebuilt.
- With more than one collection, the progress bar of `mdya update-all` and `mdya vector use` counted files past its total from the second collection on: the total was replaced for each collection, but the count carried over. Each collection now counts its files from zero.
- The `--format xml` output of `mdya search`, `mdya collection list` and `mdya status` was not well-formed XML when a value held a character XML 1.0 cannot represent, such as a form feed or ESC in a document's text. Those characters — the control characters U+0000–001F other than tab, newline and carriage return, and U+FFFE / U+FFFF — are now replaced with U+FFFD. A carriage return is now written as `&#xD;`, since an XML parser reads a literal one as a newline.
- The human and Markdown output of `mdya search` printed control characters in snippets as they were, so a document's text could move the cursor or recolor the terminal. They are now shown escaped (`\u{1b}`), except tabs.

### Security

- Bumped `serde_with` (and `serde_with_macros`) 3.20.0 → 3.22.0 in the lockfile, clearing GHSA-7gcf-g7xr-8hxj: serializing a `KeyValueMap` entry whose sequence or map was empty panicked. Upstream rates it moderate severity, a panic rather than memory unsafety. `serde_with` reaches mdya as a transitive dependency of `lance-namespace-reqwest-client` under `lancedb`; whether the affected paths are reachable here was not established. The lockfile moves to 3.22.0 rather than the 3.21.0 the advisory names because 3.21.0 fixed `KeyValueMap` alone, and 3.22.0 extends the same fix to `rust::sets_duplicate_value_is_error`, `rust::maps_duplicate_key_is_error`, `rust::sets_last_value_wins` and `rust::maps_first_key_wins`, which share the defect. The advisory was published through the GitHub Advisory Database and has no RustSec entry, so `cargo audit` did not flag it — the Dependabot alert was the only signal.
- Bumped `chacha20` 0.10.0 → 0.10.2 in the lockfile. This one is a yank rather than an advisory — it has no RustSec or GitHub identifier — and it is recorded here because `cargo audit` is what stopped on it, the same gate the entries above cleared. If you run mdya on an x86 machine whose CPU has no SSE4.1, the affected code is an illegal instruction and can abort the process; on any other CPU nothing misbehaves today. crates.io yanked 0.10.0 and 0.10.1 after the SSE2 backend was found calling `_mm_extract_epi32`, an SSE4.1 instruction, from a function declared `#[target_feature(enable = "sse2")]` — undefined behavior under Rust's rules whatever the hardware happens to do, which is how miri caught it ([RustCrypto/stream-ciphers#580](https://github.com/RustCrypto/stream-ciphers/pull/580), fixing the RNG and 64-bit-counter variants). `chacha20` picks a backend at run time in the order AVX2, SSE2, scalar, so the SSE2 code is entered only where AVX2 is absent. Three of the five release targets are x86_64; the two aarch64 targets use the NEON backend and never reach it. `chacha20` arrives through `rand`, along `pdf-extract` → `lopdf` and through `rmcp`'s `server-side-http` feature. mdya's own code does not call `rand` and rmcp 3.1.4's sources do not reference it, but `lopdf` reaches it while *reading*: authenticating the password of an RC4 revision 3 or 4 encrypted PDF fills the tail of a comparison buffer with random bytes, and `pdf-extract` authenticates every encrypted document with an empty password before extracting text. No manifest change was needed — `rand` admits `^0.10.0`, and 0.10.2 leaves the crate's dependencies, features and minimum supported Rust version untouched.
- Bumped `rustls` 0.23.40 → 0.23.45 in the lockfile, clearing RUSTSEC-2026-0285 (GHSA-2mjx-qc3c-rqvc), which upstream rates medium severity (CVSS 5.3): in a TLS 1.3 handshake, rustls accepted a handshake message sent at the wrong encryption level when it followed a key-changing message in the same record — for example, a plaintext `EncryptedExtensions` packed into the same record as the `ServerHello` — where RFC 8446 requires the connection to be terminated. The handshake transcript is still authenticated, so an attacker in the network path cannot use this to alter or complete a handshake; the effect is that a peer could send handshake messages in plaintext that should have been encrypted. In the binary, `rustls` reaches mdya through `reqwest`, which mdya uses for the Ollama backend and which `hf-hub` and `lancedb`'s `lance-namespace-reqwest-client` use as well. On every path rustls is a TLS client; nothing in mdya terminates TLS. The affected code does run in normal use: the embedding model is downloaded from `https://huggingface.co` when it is not yet cached, and the Ollama backend uses TLS when its endpoint is an `https` address (the default is `http://127.0.0.1:11434`). Building from source also runs rustls as a client, when the `lindera-ipadic` build script downloads the IPADIC dictionary. `aws-lc-rs` 1.17.0 → 1.18.1, `aws-lc-sys` 0.41.0 → 0.45.0 and `rustls-webpki` 0.103.13 → 0.103.15 moved with it: rustls 0.23.45 requires `aws-lc-rs` 1.18 and `rustls-webpki` 0.103.14 or later, and `aws-lc-rs` 1.18.1 requires `aws-lc-sys` 0.45.0. No manifest change was needed.
- Bumped `xxhash-rust` 0.8.15 → 0.8.19 in the lockfile, clearing GHSA-6g2r-675j-hx59, which upstream rates low severity: `xxh3::xxh3_64_with_secret` checked the minimum length of a caller-supplied secret only with `debug_assert!`, so in a release build a secret shorter than 136 bytes could make it read past the secret's end — undefined behavior reachable from safe code. 0.8.16 turns the check into a panic, in `xxh3_128_with_secret` as well as the 64-bit function the advisory names, and adds `SecretInput` for callers who want to validate up front. `xxhash-rust` reaches mdya only through `lance-encoding`, which `lancedb` and `lance-index` pull in. `lance-encoding` calls `xxh3_64` and hashes through `Xxh3Builder::default()`; neither takes a secret from its caller, and both use the built-in 192-byte one, so the affected functions are not on mdya's paths. The lockfile takes the latest release rather than 0.8.16 because the releases in between leave the hashes `lance-encoding` computes unchanged: `xxh3_64` is untouched, and while 0.8.17 and 0.8.18 changed how `Xxh3Builder::with_seed` derives a secret, the default builder still hashes with seed 0 and the built-in secret. No manifest change was needed — `lance-encoding` admits `^0.8.15`. As with `serde_with`, the advisory has no RustSec entry, so `cargo audit` did not flag it and the Dependabot alert was the only signal.

## [0.4.1] - 2026-08-19

### Changed

- `just check` now also runs `cargo audit`, so it requires [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit) (`cargo install cargo-audit --locked`) and network access. See the README's Development section for the offline alternative.
- The `cargo audit` workflow is now a daily scheduled monitor that opens an issue labelled `security-advisory` when the scan fails, replacing the previous weekly + per-dependency-change pull request trigger. The advisory database changes outside this repository, so a pull request check reported on `main`'s state rather than on the pull request.
- Bumped `lancedb` 0.30.0 → 0.37.1 and `lance-index` 7.0.0 → 10.0.0 in lockstep. lancedb 0.37 requires the whole `lance-*` ecosystem at one exact version, so neither pin can move on its own. `arrow-array` / `arrow-schema` stay on 58, which both new versions still require. The move to lancedb 0.31 had been held back because it requires `lance-testing` 8.0.0, which added `pprof` (with the `flamegraph` feature) as a normal dependency, and behind it `inferno` → `quick-xml` 0.26 — a version RUSTSEC-2026-0194 / 0195 affect, both patched in 0.41.0. lancedb 0.33.0 moved `lance-testing` to dev-dependencies, so `lance-testing` drops out of the tree here and `pprof` never enters it.
- Bumped `serde-saphyr` 0.0.29 → 1.x (resolved at 1.1.0). Upstream adopted semver at 1.0, so the requirement is now `"1"` instead of an effectively exact `0.0.x` pin. One upstream behaviour change can reach a hand-edited `config.yml`: explicit YAML core tags are enforced strictly from 1.0 on, so a scalar written as `!!str 42` is no longer coerced to an integer. The `config.yml` mdya generates uses no explicit tags, so no action is required unless you added some yourself.
- Bumped `candle-core` / `candle-nn` / `candle-transformers` 0.10.1 → 0.11.0 (lockstep). The historical `=0.10.1` exact-pin was in place because crates.io candle-core 0.10.2 had been published without a matching git tag; upstream tagging has since caught up (`0.10.2` and `0.11.0` both tagged), so we lift the hold and move all three crates in one step.
- `just check` now verifies that cargo-audit is installed before it starts, instead of after the full test run. Missing the tool used to surface only once `cargo test --workspace` had finished, which on a cold cache meant waiting half an hour to be told about a one-line install.
- The Development section of the README now states that `just check` needs the build-from-source prerequisites (Rust, `just`, `protoc`) as well as cargo-audit. CONTRIBUTING.md points here for what `just check` needs installed, and that list was previously incomplete.

### Fixed

- A fresh clone can now run `cargo build`, `cargo clippy` and `cargo test` across the workspace without first downloading the 16 MB Noto Sans CJK JP font. The `xtask-generate-test-pdfs` helper embedded the font with `include_bytes!`, which turned a `.gitignore`d file into a compile-time input for a workspace member and stopped `just check` before a single test ran. It now reads the font at run time, so only regenerating the PDF fixtures needs the file present.

### Security

- Bumped `rkyv` (and `rkyv_derive`) 0.8.16 → 0.8.18 in the lockfile, clearing RUSTSEC-2026-0233, RUSTSEC-2026-0234 and RUSTSEC-2026-0235: a crafted archive could trigger a use-after-free during deserialization, and insufficient validation of archives containing hash tables or `Rc`/`Arc` could read out of bounds. rkyv reaches mdya through the tokenizer dictionaries that back full-text search (`lindera-dictionary` → `lindera` → `lance-tokenizer` → `lancedb`), so the archive it deserializes is the IPADIC dictionary compiled into the binary rather than anything a user supplies. `event-listener` 5.4.1 → 5.4.2 moved at the same time for RUSTSEC-2026-0221, which the advisory marks informational (unsound) rather than a vulnerability.
- Bumped `h2` 0.4.14 → 0.4.16 in the lockfile, clearing RUSTSEC-2026-0258 (GHSA-q83h-524g-xf6h): empty HTTP/2 DATA frames were accepted and queued without limit, so a stream that is not actively drained could grow memory without bound, or panic once the length overflowed. Upstream rates it low severity, denial of service only. h2 sits on both sides of mdya's HTTP surface — the server behind `mdya mcp --http`, which binds to loopback (`127.0.0.1:8000`) unless `--addr` says otherwise, and the client that fetches embedding models. No manifest change was needed, because `hyper` already admitted the patched release.
- Accepted two more upstream `unmaintained` advisories as WONTFIX in `.cargo/audit.toml`: RUSTSEC-2026-0192 (`ttf-parser`) and RUSTSEC-2026-0206 (`rustybuzz`). Both are informational rather than vulnerabilities. `ttf-parser` is in the released binary, reached through `pdf-extract` → `lopdf` for PDF ingest; `rustybuzz` is not, reaching only the `xtask` fixture generator through `krilla`. See the file header and `SECURITY.md` for the policy.

## [0.4.0] - 2026-06-21

### Added

- Two more on-device embedding presets, selectable through `embedding.model` in `config.yml` alongside the default `cl-nagoya/ruri-v3-30m` (256-dim): `sentence-transformers/all-MiniLM-L6-v2` (384-dim) and `google/embeddinggemma-300m` (768-dim, gated on Hugging Face — requires accepting Google's Gemma terms and supplying an access token). An unrecognized `embedding.model` is now rejected at startup with the list of supported values, before any indexing begins.
- Published to [crates.io](https://crates.io/crates/mdya): `cargo install mdya` now works alongside the prebuilt installers. This path builds from source and requires `protoc`.
- A `get` section in `config.yml` with `cli_max_bytes` and `mcp_max_bytes` (both default 1 MiB; `0` disables) capping the size of a full document returned by `mdya get` and the MCP `get_document` tool. `mdya get` gained a `-f` / `--no-size-limit` flag to bypass the cap for a single run.

### Changed

- Markdown chunking now fills each chunk with whole blocks (paragraphs, list items, and so on) up to the target size, splitting only at block boundaries.
- `mdya get` and the MCP `get_document` tool now refuse to return a full document larger than 1 MiB by default, to avoid flooding a terminal or an LLM's context budget. Restore the previous unlimited behavior by passing `-f` / `--no-size-limit` to `mdya get`, or by raising / disabling (`0`) the new `get.cli_max_bytes` / `get.mcp_max_bytes` caps in `config.yml`. Single-chunk reads (`--chunk` / the MCP `chunk` parameter) are not size-checked.

### Fixed

- `mdya vector use` now rebuilds a stale vector index even when `config.yml` already names the target model, instead of skipping the rebuild.

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

[Unreleased]: https://github.com/yoshihirosuzuki/mdya/compare/v0.4.2...HEAD
[0.4.2]: https://github.com/yoshihirosuzuki/mdya/releases/tag/v0.4.2
[0.4.1]: https://github.com/yoshihirosuzuki/mdya/releases/tag/v0.4.1
[0.4.0]: https://github.com/yoshihirosuzuki/mdya/releases/tag/v0.4.0
[0.3.2]: https://github.com/yoshihirosuzuki/mdya/releases/tag/v0.3.2
[0.3.1]: https://github.com/yoshihirosuzuki/mdya/releases/tag/v0.3.1
[0.3.0]: https://github.com/yoshihirosuzuki/mdya/releases/tag/v0.3.0
