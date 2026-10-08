# Command reference

This page lists every mdya subcommand. All subcommands accept the following global flags.

## Global flags

| Flag | Purpose | Default |
|---|---|---|
| `--config-dir <PATH>` | Override the configuration directory | `$HOME/.mdya/` |
| `--model-cache-dir <PATH>` | Override the embedding model cache directory | `$HOME/.mdya-models/` |
| `--log-level <LEVEL>` | Log verbosity (`trace` / `debug` / `info` / `warn` / `error`). Also settable via the `RUST_LOG` environment variable | `warn` |
| `--log-format <FORMAT>` | Log format (`compact` / `pretty` / `json`) | `compact` |
| `--no-color` | Suppress terminal colors. Also settable via the `NO_COLOR` environment variable | TTY auto-detect |
| `-h, --help` | Show help | — |
| `-V, --version` | Show version | — |

All logs go to stderr. stdout is reserved for data — search results, MCP protocol messages, and so on.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Application error (I/O, configuration mismatch, search failure, etc.) |
| `2` | Argument parse error |
| `101` | Internal error (panic) |
| `137` | Hard termination after exceeding the memory limit (see [`memory_limit_mb` in configuration.md](configuration.md#memory_limit_mb)) |

---

## `mdya init`

Initializes `~/.mdya/`.

```sh
mdya init
```

- Creates `config.yml` with default values (leaves it alone if it already exists).
- Creates `index/` and `lance-models/` (leaves them alone if they already exist).
- The embedding model cache lives in `~/.mdya-models/` (changeable via `--model-cache-dir`) and is downloaded automatically on the first `mdya update-all` or vector / hybrid search. `mdya init` does not create that directory.

It is safe to run any number of times. To start over, remove `~/.mdya/` by hand and re-run.

---

## `mdya collection add <path>`

Registers a directory as a collection.

```sh
mdya collection add <path> [--name <NAME>] [--description <TEXT>]
```

| Argument / option | Purpose |
|---|---|
| `<path>` (required) | The directory to register. `~/...` is expanded |
| `--name <NAME>` | Override the collection name (default: basename of `<path>`) |
| `--description <TEXT>` | Human-readable description (shown by `mdya collection list`) |

This only appends one entry to the `collections` section of `config.yml`. No indexing happens here. Indexing runs when you call `mdya update-all`.

Examples:

```sh
mdya collection add ~/notes
mdya collection add ~/work --name work-docs --description "Work notes"
```

---

## `mdya collection list`

Lists registered collections.

```sh
mdya collection list [--format <FORMAT>]
```

- `--format` is one of `human` (default) / `json` / `md` / `xml`.
- For each collection it shows `name` / `path` / `description` / `document_count` (number of indexed documents).
- `document_count` reflects the index, so collections you have not yet run `mdya update-all` against report `0`.

```sh
$ mdya collection list
notes  ~/notes        Personal notes  42 docs
work   ~/work          (no desc)        0 docs
```

Using `json` gives you a script-friendly envelope.

```sh
$ mdya collection list --format json
{
  "collections": [
    { "name": "notes", "path": "~/notes", "description": "Personal notes", "document_count": 42 }
  ]
}
```

---

## `mdya update-all`

Walks every registered collection, ingests new and changed `.md` / `.markdown` / `.pdf` files, and removes index entries for files that are gone from the walk — deleted or newly excluded by `.gitignore`.

```sh
mdya update-all
```

- Files with the `.md`, `.markdown`, or `.pdf` extension are indexed. `.pdf` is converted to plain text at ingest time and flows through the same chunker and embedder as Markdown.
- Symbolic links under a collection root are not followed (see [`collections` in configuration.md](configuration.md#collections)).
- Paths excluded by `.gitignore` are not indexed when the collection root is inside a git repository (see [`collections` in configuration.md](configuration.md#collections)).
- Parallelism is controlled by `runtime.embed_parallelism`.
- Progress is shown on stderr, in a form that depends on whether a progress bar can be shown there (see [Progress display](#progress-display)).
- At the end, a one-line summary is written to stderr. Nothing is written to stdout.

```
Indexed 43 documents (new: 5, updated: 3, skipped: 34, removed: 0, failed: 1).
```

What each counter means:

- `new` — files ingested for the first time
- `updated` — files re-ingested because their content changed
- `skipped` — files left alone because nothing changed
- `removed` — index entries dropped because the file disappeared or is now excluded by `.gitignore`
- `failed` — files that failed to ingest

When `failed > 0`, the command exits with `1` (the summary is still printed).

### Progress display

When a progress bar can be shown on stderr, it is shown along with one line per file in progress. Once a file's chunks are being embedded, its line shows the chunks done and the total after the file name. After every file is processed, `Updating search indexes` is shown while the search indexes are updated.

When a progress bar cannot be shown, the state at that moment is printed as one line when indexing starts and then every 10 seconds. That is the case when stderr is not a terminal (redirected to a file, piped, a CI log, and so on), and when the `TERM` environment variable is `dumb` (or, outside Windows, unset).

```
Indexing 'notes': 12/340 files; in progress: docs/big.pdf (320/12000 chunks), memo.md
```

The file counts are for the collection being processed and start from 0 for each collection. `in progress:` lists the files being processed, with the chunk progress of each file whose chunks are being embedded. While the search indexes are updated after every file is processed, the status line reads `Updating search indexes`.

---

## `mdya search fts|vector|hybrid <query>`

Searches in one of three modes. Arguments and options are identical across all three.

```sh
mdya search fts    <query> [-c <NAMES>...] [-n <N>] [--chunks] [--format <FORMAT>]
mdya search vector <query> [-c <NAMES>...] [-n <N>] [--chunks] [--format <FORMAT>]
mdya search hybrid <query> [-c <NAMES>...] [-n <N>] [--chunks] [--format <FORMAT>]
```

| Argument / option | Purpose | Default |
|---|---|---|
| `<query>` (required) | The search query string. An empty string is an error | — |
| `-c, --collections <NAMES>` | Restrict to specific collections. CSV (`-c notes,work`) or repeated (`-c notes -c work`) for OR. Space-separated (`-c notes work`) is not accepted | All collections |
| `-n, --limit <N>` | Top N hits | `20` |
| `--chunks` | Return hits at chunk granularity. When off (default), returns one aggregated hit per document; when on, returns one hit per matched chunk within each document | off |
| `--format <FORMAT>` | Output format (`human` / `json` / `md` / `xml`) | `human` |

### The three modes

- `fts` — BM25 full-text search. Looks at term matches. Supports Japanese morphological analysis.
- `vector` — cosine-similarity vector search. Looks at semantic proximity. The first invocation downloads the embedding model (about 140 MB) into `~/.mdya-models/`.
- `hybrid` — fuses the `fts` and `vector` result lists via Reciprocal Rank Fusion (RRF).

The numeric range of `score` differs per mode:

- `fts` — BM25 score (unbounded; the raw value has no absolute meaning)
- `vector` — cosine similarity in `[0, 1]`
- `hybrid` — RRF sum (upper bound ≈ `0.033`)

Do not compare scores across modes directly.

### Output formats

#### `--format human` (default)

```
notes/release.md  score=0.812  chunks=4
  > The release procedure is as follows
  > 1. Update the version number
---
notes/checklist.md  score=0.751  chunks=1
  > Pre-release checklist
---
2 doc hits (showing 20 max)
```

#### `--format json`

A machine-friendly envelope. Feeds into `jq` and similar tools.

```sh
mdya search fts "release" --format json | jq '.hits[].path'
```

Envelope structure:

```json
{
  "query": "release",
  "mode": "fts",
  "level": "doc",
  "collections": ["notes"],
  "limit": 20,
  "total": 17,
  "hits": [
    {
      "collection": "notes",
      "path": "release.md",
      "score": 0.812,
      "snippet": "...",
      "matched_chunks": 3,
      "chunk_count": 4
    }
  ]
}
```

- `level` is the hit granularity. At the default `doc`, hits are aggregated per document and each hit's `matched_chunks` counts how many chunks within the document matched. Passing `--chunks` sets `level: "chunk"` and each hit carries `chunk_sequence` (the 0-indexed chunk number).
- `chunk_count` is the total number of chunks in the hit's document, present on hits of either granularity. It bounds `mdya get --chunk <N> --chunk-end <M>` when you read around a hit or onward through the document.
- `total` matches the length of `hits` (after `limit` truncation). With `level: "doc"` it counts documents; with `level: "chunk"` it counts chunks.
- Hits are ordered by `score`, descending.
- Scripts must check the exit code. When a `json`-formatted search fails, stdout is empty (not `{"hits":[]}`), so judging success by stdout presence alone is misleading.

#### `--format md`

A Markdown-formatted view suitable for both humans and agents. Multi-line snippets are rendered as blockquotes, which improves readability. Good for pasting into an LLM context.

#### `--format xml`

XML that mirrors the JSON structure 1:1.

### Error behavior

When a search fails under a non-`human` format, stdout is empty and the error is printed to stderr in `human` format. Scripts should use the exit code to determine success or failure.

---

## `mdya get <collection> <path>`

Prints the original text of a document to stdout. Useful when you want to retrieve the source document for a search hit.

```sh
mdya get <collection> <path> [--chunk <N> [--chunk-end <M>]] [-f]
```

| Argument | Purpose |
|---|---|
| `<collection>` (required) | Collection name (must be declared in `config.yml`) |
| `<path>` (required) | Path relative to the collection root |

| Option | Purpose |
|---|---|
| `--chunk <N>` | Print only the original text of chunk `N` (0-indexed) instead of the full document |
| `--chunk-end <M>` | With `--chunk <N>`, print chunks `N` through `M` (inclusive) as one contiguous piece of the original text. `M` must be `>= N`; an `M` past the last chunk reads to the end of the document |
| `-f`, `--no-size-limit` | Print the output regardless of size, bypassing the `get.cli_max_bytes` cap |

Examples:

```sh
mdya get notes release.md
mdya get notes release.md --chunk 3                 # just the chunk that matched
mdya get notes release.md --chunk 2 --chunk-end 4   # with its neighbours
```

It returns the text stored in the index, not the live filesystem contents. Documents not yet ingested (e.g. `mdya update-all` has not been run) cannot be retrieved.

`--chunk` returns the part of the original text the chunk covers, formatting included: heading markers, link targets, HTML, and so on. Chunk ranges cover the document without gaps: `--chunk 0 --chunk-end <last>` returns the whole document, and within one range read no text repeats between adjacent chunks. Chunks cut from one long block (a long paragraph or code block, or a PDF) overlap their neighbours slightly, though: when separate reads meet inside such a block, joining them repeats a little text at the seam, whether they are single chunks or ranges. An `N` past the last chunk is an error. A search hit's `chunk_count` tells you how many chunks the document has.

By default `mdya get` stops with an error when its output exceeds `get.cli_max_bytes` (1 MiB by default; see [`cli_max_bytes` in configuration.md](configuration.md#cli_max_bytes)), so an accidental fetch of a very large file does not flood your terminal. This applies to full documents and `--chunk` reads alike. Pass `-f` / `--no-size-limit` to print it anyway — useful when you redirect or pipe a large output on purpose.

On an index built by an older mdya (before chunk ranges were recorded), `--chunk` stops with an error that names the command to rebuild the index; full-document reads keep working. Rebuild with [`mdya vector use`](#when-the-index-format-is-outdated).

---

## `mdya status`

Reports index state.

```sh
mdya status [--format <FORMAT>]
```

| Option | Purpose | Default |
|---|---|---|
| `--format <FORMAT>` | Output format (`human` / `json` / `md` / `xml`) | `human` |

Returned information:

- `version` — mdya version
- `embedding_model` — model ID used when building the index
- `vector_dim` — vector dimensionality
- `collections` — number of registered collections
- `chunks` — total number of chunks in the index
- `sources` — total number of documents in the index

Assumes `mdya init` has been run. If the directory has not been initialized, the command fails.

```sh
$ mdya status --format json
{
  "version": "0.0.0",
  "embedding_model": "cl-nagoya/ruri-v3-30m",
  "vector_dim": 256,
  "collections": 3,
  "chunks": 12043,
  "sources": 312
}
```

---

## `mdya vector use <model>`

Switches the embedding model. This is a **destructive operation**.

```sh
mdya vector use <model> [--yes]
```

| Argument / option | Purpose |
|---|---|
| `<model>` (required) | The new model ID. Examples: `cl-nagoya/ruri-v3-30m` / `ollama:nomic-embed-text` |
| `-y, --yes` | Skip the confirmation prompt (required on non-interactive stdin) |

### What happens

1. Rewrites `embedding.model` in `config.yml` to the new model.
2. Drops the vector table inside the index.
3. Walks every registered collection and re-embeds documents with the new model (same walk as `update-all`, so `.gitignore` exclusions apply). Progress is shown the same way as for `update-all` (see [Progress display](#progress-display)).

The document index (the source text returned by `mdya get`) is model-independent and stays in place. Only the vector portion is recomputed.

### Confirmation prompt

On an interactive terminal it shows what is about to change, then asks `[y/N]`. Entering `y` or `yes` proceeds. Anything else (including an empty answer) aborts.

```
This will switch the embedding model:
  cl-nagoya/ruri-v3-30m -> ollama:nomic-embed-text (dim 768)
The chunks index will be DROPPED and 3 collection(s) re-embedded from scratch.
Proceed? [y/N]:
```

For scripts and other non-interactive stdin contexts, pass `--yes`. Running without `--yes` on a non-interactive stdin is rejected for safety.

### Completion summary

At the end, a line like the following is written to stderr. Nothing is written to stdout.

```
Switched embedding model to 'ollama:nomic-embed-text'. Re-embedded 312 document(s) (removed: 0, failed: 0).
```

When `failed > 0`, the command exits with `1`. The rest can be resumed by re-running `mdya update-all` normally.

### When the index format is outdated

On an index built by an older mdya (before chunk ranges were recorded), `mdya update-all`, `mdya search`, and `mdya get --chunk` stop with an error that suggests:

```sh
mdya vector use <current model>
```

This keeps the model, recreates the index's vector table, and re-ingests every document; the model in `config.yml` stays as it is. It asks for confirmation just like a model switch, worded as:

```
This will rebuild the index for 'cl-nagoya/ruri-v3-30m' (dim 256).
The chunks index will be DROPPED and 3 collection(s) re-embedded from scratch.
Proceed? [y/N]:
```

When it finishes it prints `Rebuilt the index for 'cl-nagoya/ruri-v3-30m'. Re-embedded 312 document(s) (removed: 0, failed: 0).` When the model and the index format are both current, it does nothing.

If `mdya mcp` is running, restart it after the rebuild: a running server keeps the index format it read at startup.

### Using Ollama

Specifying the `ollama:<model>` form uses the Ollama embedding API. Make sure Ollama is running and that the model has been pulled (e.g. `ollama pull <model>`) ahead of time.

### Using a gated model

Switching to the gated `google/embeddinggemma-300m` first requires a one-time Hugging Face license acceptance and login — see [`embedding` in configuration.md](configuration.md#embedding).

---

## `mdya mcp`

Starts the MCP (Model Context Protocol) server. This lets clients such as Claude Code call the search tools.

See [`mcp.md`](mcp.md) for details.

```sh
mdya mcp              # start over stdio (default)
mdya mcp --http       # start over HTTP (foreground daemon)
mdya mcp --http --addr 127.0.0.1:9000  # specify bind address
```

---

## `mdya version`

Prints the mdya version on a single line.

```sh
$ mdya version
mdya v0.0.0
```

Provided as a stable surface for scripts and MCP clients to read.
