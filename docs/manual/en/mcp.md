# MCP server

mdya can run as an MCP (Model Context Protocol) server. MCP clients such as Claude Code or Claude Desktop can then call its search tools.

## Launching

```sh
mdya mcp                                  # stdio (default)
mdya mcp --http                           # HTTP (foreground daemon)
mdya mcp --http --addr 127.0.0.1:9000     # specify the bind address
```

| Mode | Use case | Notes |
|---|---|---|
| stdio (default) | Editor integration (Claude Code, etc.) | stdout is reserved for the MCP protocol; logs go to stderr |
| `--http` | Background daemon | A foreground process. Background it with `&` / `nohup` / systemd / etc. from the shell |

The `--http` bind address defaults to `127.0.0.1:8000` (loopback). Remote exposure is out of scope; the server rejects Host headers from non-loopback addresses.

Only one `--http` daemon can run per configuration directory (a `~/.mdya/mcp.pid` lock guards it). A second instance is rejected.

The stdio mode is designed for editor integration. It assumes the client manages the process lifetime.

## Registering with Claude Code

```sh
claude mcp add mdya -- mdya mcp
```

This registers the MCP server under the name `mdya` and makes its tools available.

## Provided tools

The mdya MCP server provides the following tools.

### Search tool

| Tool | Description |
|---|---|
| `search` | Search Markdown collections; `mode` selects the backend (BM25 / vector / hybrid) |

Input schema:

```json
{
  "query": "release plan",
  "mode": "hybrid",
  "k": 20,
  "collections": ["notes"],
  "level": "doc"
}
```

- `query` (required) — search query string. Empty or whitespace-only is an error.
- `mode` (optional, default `"hybrid"`) — search backend: `"fts"` (BM25 keyword/phrase), `"vector"` (cosine semantic), or `"hybrid"` (both, fused with RRF).
- `k` (optional, default `20`) — top N hits. `0` is an error.
- `collections` (optional) — filter by collection name. Omitted or empty array means all collections. Unknown collection names are an error.
- `level` (optional, default `"doc"`) — hit granularity. `"doc"` returns aggregated hits per document; `"chunk"` returns one hit per matched chunk.

The output matches the CLI's `--format json` envelope.

```json
{
  "query": "release plan",
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

At `level: "doc"` (default), hits are aggregated per document and each hit's `matched_chunks` counts how many chunks within the document matched. Passing `level: "chunk"` returns hits at chunk granularity, and each hit carries `chunk_sequence` (the 0-indexed chunk number). At either granularity, each hit's `chunk_count` is the total number of chunks in its document.

The numeric range of `score` differs per mode (see [the three modes in commands.md](commands.md#the-three-modes)).

### Document retrieval tool

| Tool | Description |
|---|---|
| `get_document` | Fetch the original text of one document |

Input:

```json
{
  "collection": "notes",
  "path": "release.md"
}
```

Output:

```json
{
  "collection": "notes",
  "path": "release.md",
  "content": "..."
}
```

- `collection` (required) — collection name (must be declared in `config.yml`).
- `path` (required) — document path relative to the collection root.
- `chunk` (optional) — a 0-indexed `chunk_sequence`. When given, the original text that chunk covers is returned instead of the full document; omit it for the faithful full document.
- `chunk_end` (optional) — with `chunk`, return chunks `chunk` through `chunk_end` (inclusive) as one contiguous piece of the original text. Must be `>= chunk`; a value past the last chunk reads to the end of the document.

You can pass the `collection` and `path` from a search hit directly to retrieve its source text, or pass a hit's `chunk_sequence` as `chunk` to fetch just that chunk. To read its neighbours too, pass a `chunk` below the hit's number and a `chunk_end` above it. A chunk's original text keeps its formatting: heading markers, link targets, HTML, and so on. Chunk ranges cover the document without gaps, so within one range read no text repeats between adjacent chunks. Chunks cut from one long block (a long paragraph or code block, or a PDF) overlap their neighbours slightly, though: when separate reads meet inside such a block, joining them repeats a little text at the seam.

A document whose full text exceeds `get.mcp_max_bytes` can usually still be read piece by piece with `chunk` / `chunk_end` ranges that fit under the cap. A part where a single chunk's range alone exceeds the cap — large content with no body text, such as a huge HTML block — cannot be read this way. A search hit's `chunk_count` tells you how many chunks the document has.

### Index introspection tool

| Tool | Description |
|---|---|
| `list_collections` | List registered collections |

It takes no input. The output shape matches `mdya collection list --format json` (see [commands.md](commands.md)).

## Error behavior

When a tool call fails, a structured error is returned.

```json
{
  "code": "unknown_collection",
  "message": "unknown collection: 'foo'",
  "details": { "collection": "foo" }
}
```

| `code` | Trigger |
|---|---|
| `empty_query` | `query` is empty or whitespace-only |
| `invalid_limit` | `k` is `0` |
| `unknown_collection` | An unknown collection name was given |
| `not_found` | `get_document` found no matching document, or the requested `chunk` is out of range |
| `invalid_chunk_range` | `get_document` got `chunk_end` without `chunk`, or a `chunk_end` smaller than `chunk` |
| `payload_too_large` | A `get_document` response (full document or `chunk` / `chunk_end` range) exceeded `get.mcp_max_bytes`. `details` carries `size_bytes` / `limit_bytes`; narrow the `chunk` / `chunk_end` range to fit |
| `index_outdated` | The index was built by an older mdya, before chunk ranges were recorded. Returned by `search` and by `get_document` with `chunk`. Rebuild it with `mdya vector use <model>`, using the model in `details.embedding_model`. Full-document `get_document` keeps working. Restart the MCP server after the rebuild |
| `schema_metadata_missing` | The index is uninitialized or corrupted |
| `internal` | I/O, embedding, configuration-load, or other failure |

Clients can branch on `code`. `details` carries code-specific extra information.

## Logging

All `mdya mcp` logs go to stderr. In stdio mode stdout is occupied by the MCP protocol channel, so mixing logs into stdout would break the protocol.

When backgrounding the `--http` daemon and you want logs written to a file, redirect from the shell.

```sh
nohup mdya mcp --http > ~/.mdya/mcp.log 2>&1 &
```
