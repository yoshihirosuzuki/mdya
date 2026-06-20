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
      "matched_chunks": 3
    }
  ]
}
```

At `level: "doc"` (default), hits are aggregated per document and each hit's `matched_chunks` counts how many chunks within the document matched. Passing `level: "chunk"` returns hits at chunk granularity, and each hit carries `chunk_sequence` (the 0-indexed chunk number).

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
- `chunk` (optional) — a 0-indexed `chunk_sequence`. When given, only that single chunk's body is returned instead of the full document; omit it for the faithful full document.

You can pass the `collection` and `path` from a search hit directly to retrieve its source text, or pass a hit's `chunk_sequence` as `chunk` to fetch just that chunk.

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
| `payload_too_large` | A full `get_document` response exceeded `get.mcp_max_bytes`. `details` carries `size_bytes` / `limit_bytes`; fetch a single `chunk` to narrow it |
| `schema_metadata_missing` | The index is uninitialized or corrupted |
| `internal` | I/O, embedding, configuration-load, or other failure |

Clients can branch on `code`. `details` carries code-specific extra information.

## Logging

All `mdya mcp` logs go to stderr. In stdio mode stdout is occupied by the MCP protocol channel, so mixing logs into stdout would break the protocol.

When backgrounding the `--http` daemon and you want logs written to a file, redirect from the shell.

```sh
nohup mdya mcp --http > ~/.mdya/mcp.log 2>&1 &
```
