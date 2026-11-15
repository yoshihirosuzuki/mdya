# The MCP server

`mdya mcp` runs as an MCP server and lets an AI assistant (such as Claude Code) search the documents in mdya.

## What MCP can and cannot do

There are four MCP tools, listed below. All of them only read, and none of them changes anything in mdya.

| Tool | What it does |
|---|---|
| `list_collections` | List the collections |
| `search` | Search |
| `get_document` | Get a document |
| `get_version` | Show the mdya version |

Operations such as adding a collection, ingesting, removing a document, and changing settings cannot be done through MCP. Use the CLI commands for them ([Commands](commands.md)). Documents ingested with the CLI while the MCP server is running can be searched from the next call on, without restarting the server.

## Starting the server

| How it runs | How it is started |
|---|---|
| stdio | The MCP client starts `mdya mcp` and talks with it over standard input and standard output |
| HTTP | You keep `mdya mcp --http` running, and the MCP client connects to it over HTTP |

With stdio, standard output is used only for MCP communication. Logs go to stderr.

With HTTP, the server listens on the path `/mcp`. The address to listen on is `127.0.0.1:8000` by default, and you can change it with `--address`. When the server starts, it writes where it is listening to stderr.

```
$ mdya mcp --http --address 127.0.0.1:18765
Listening on http://127.0.0.1:18765/mcp
```

The HTTP server is meant to be used from within the computer that runs mdya. It refuses requests addressed to a host name other than a loopback one. A request with an `Origin` header is accepted only when its scheme is `http` or `https` and its host is loopback; otherwise, it is refused with HTTP 403. A request without an `Origin` header is accepted. This is so that mdya does not accept requests that a browser sends to it from a page of a site outside the computer.

## Registering with an AI assistant

### Claude Code

To register it with stdio:

```
claude mcp add --transport stdio mdya -- mdya mcp
```

To use a settings and index directory other than the default, put `--data-dir` before `mcp`.

```
claude mcp add --transport stdio mdya -- mdya --data-dir /Users/me/work-mdya mcp
```

To connect to an HTTP server that you keep running:

```
claude mcp add --transport http mdya http://127.0.0.1:8000/mcp
```

### Other MCP clients

With stdio, use `mdya` as the command to start and `mcp` as its arguments. Put global options such as `--data-dir` in the arguments before `mcp`. With HTTP, use `http://127.0.0.1:8000/mcp` as the URL to connect to (or, if you changed `--address`, that address).

## Tools

Each tool gives, in its tool definition, an output schema that describes the structure of its result. On success, it returns a result whose structure matches that schema.

### `list_collections`

It takes no input. The result has the same structure as `mdya collection list --json` ([Commands: Listing collections](commands.md#listing-collections-mdya-collection-list)). The descriptions of the collections can be used to decide which collections to search.

### `search`

| Input | Meaning | When omitted |
|---|---|---|
| `query` | What to search for. Input that is only white space is not accepted | Cannot be omitted |
| `mode` | One of `fts`, `vector`, and `hybrid` | `hybrid` |
| `collections` | An array of the names of the collections to search | All the collections (an empty array is the same) |
| `limit` | The most hits to return. From 1 to 9007199254740991 | 20 |
| `unit` | `document` or `chunk` | `document` |

The result has the same structure as `mdya search --json` ([Commands: Searching](commands.md#searching-mdya-search)). You can pass each hit's `collection`, `key`, and `chunk_index` to `get_document` to read the document.

### `get_document`

| Input | Meaning | When omitted |
|---|---|---|
| `collection` | The collection name | Cannot be omitted |
| `key` | The key of the document | Cannot be omitted |
| `chunk_index` | The chunk index of the first chunk | Returns the whole body of the document |
| `end_chunk_index` | The chunk index of the last chunk (this chunk is included). Must be `chunk_index` or larger. If it is past the last chunk, returns the body up to the end of the document | Returns only the chunk at `chunk_index` |

The result has `collection`, `key`, and `text` (the text to return). When the text to return is over the MCP limit (`get.mcp_limit_bytes`, 1 MiB by default; [Settings](configuration.md#output-size-limit-for-getting-documents)), an error is returned. Give a chunk range and get the document in parts.

```json
{"collection":"notes","key":"release.md","text":"# リリースの手順\n\nタグを打つ前に、変更履歴を書き、バージョン番号を上げます。\n\n"}
```

### `get_version`

It takes no input. The result has the form `{"version":"mdya X.Y.Z"}`. It returns the version of the mdya that is running as the MCP server.

#### Hiding the version

With `mdya config set mcp.show_version false`, mdya does not show its version over MCP. The result of `get_version` becomes `{"version":"mdya"}`, and the `version` in the server information returned at MCP initialization also becomes `mdya`. This has no effect on `mdya --version` and `mdya status` in the CLI.

## Errors

A tool error is returned as an MCP tool execution error (`isError: true`). Its content is a JSON object of the following form. The same content is also included as text.

```json
{"code":"collection_not_found","message":"Error: the collection \"nosuch\" does not exist\n\nHow to fix:\n    Check the collection name in the list of collections (mdya collection list, or the list_collections tool).","details":{"collection":"nosuch"}}
```

- `code`: The kind of error. It is one of those in the table below. Treat a `code` you do not know the same as `internal`.
- `message`: The same explanation (the error message, the cause, and how to fix) that the CLI writes to stderr when the same error happens there.
- `details`: Information for fixing the input and calling again.

| `code` | Errors it covers | `details` |
|---|---|---|
| `invalid_input` | [The query is empty](errors.md#the-query-is-empty), [The limit is zero](errors.md#the-limit-is-zero), [The chunk does not exist](errors.md#the-chunk-does-not-exist), [The chunk range is not valid](errors.md#the-chunk-range-is-not-valid), [The query is longer than the embedding model's context length](errors.md#the-query-is-longer-than-the-embedding-models-context-length), an input value does not match the input schema (an unknown search mode, a missing required input, and so on) | `input`: the name of the input that is not valid |
| `collection_not_found` | [The collection does not exist](errors.md#the-collection-does-not-exist) | `collection`: the collection name |
| `document_not_found` | [The document does not exist](errors.md#the-document-does-not-exist) | `collection`: the collection name, `key`: the key |
| `output_too_large` | [The output is too large](errors.md#the-output-is-too-large) | `size_bytes`: the size of what is to be returned, `limit_bytes`: the MCP limit |
| `embedding_model_unavailable` | [The embedding model cannot be used](errors.md#the-embedding-model-cannot-be-used) | None (an empty object) |
| `internal` | [The index or settings cannot be read](errors.md#the-index-or-settings-cannot-be-read), [Unexpected failure](errors.md#unexpected-failure) | None (an empty object) |

## When the settings change

The MCP server keeps using the settings it had when it started. Settings changed with `mdya config set` take effect when the server is restarted.

When you change the embedding model setting, restart the server. If you do not, the server makes the embeddings of queries with the previous embedding model, and after you rebuild the embeddings with the CLI, vector search no longer returns any hits ([Some embeddings were made with another model than the configured one](errors.md#some-embeddings-were-made-with-another-model-than-the-configured-one)).

## Logs

The MCP server's logs go to stderr. To keep the logs in a file, redirect stderr to a file in the shell ([Settings: Logs and color](configuration.md#logs-and-color)).
