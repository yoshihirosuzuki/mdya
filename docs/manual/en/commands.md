# Commands

This page shows how to write the commands of mdya's CLI, and what they output. In the example outputs, locations have been replaced with ones under `/Users/me`, and the mdya version with `X.Y.Z`.

## List of commands

| Task | Command |
|---|---|
| Add a collection | [`mdya collection add`](#adding-a-collection-mdya-collection-add) |
| Remove a collection | [`mdya collection remove`](#removing-a-collection-mdya-collection-remove) |
| List the collections | [`mdya collection list`](#listing-collections-mdya-collection-list) |
| Change a collection's location or description | [`mdya collection set`](#changing-a-collections-location-or-description-mdya-collection-set) |
| Update the collection you specify | [`mdya update <collection name>`](#updating-collections-mdya-update) |
| Update all the collections | [`mdya update --all`](#updating-collections-mdya-update) |
| Pass in a document's content and ingest it | [`mdya put`](#ingesting-a-document-you-pass-in-mdya-put) |
| Remove a document you specify | [`mdya remove`](#removing-a-document-mdya-remove) |
| Search | [`mdya search`](#searching-mdya-search) |
| Get a document | [`mdya get`](#getting-a-document-mdya-get) |
| Show the state of the index | [`mdya status`](#showing-the-state-of-the-index-mdya-status) |
| Show the settings | [`mdya config list`](#showing-the-settings-mdya-config-list) |
| Change the settings | [`mdya config set`, `mdya config reset`](#changing-settings-mdya-config-set-mdya-config-reset) |
| Rebuild the embeddings | [`mdya embedding rebuild`](#rebuilding-the-embeddings-mdya-embedding-rebuild) |
| Maintain the index | [`mdya optimize`](#maintaining-the-index-mdya-optimize) |
| Start the MCP server | [`mdya mcp`](#starting-the-mcp-server-mdya-mcp) |
| Show the mdya version | [`mdya --version`, `mdya -v`](#showing-the-mdya-version-mdya---version-mdya--v) |
| Show help | [`--help`, `-h`](#showing-help---help--h) |

## Common to all commands

### Global options

Write the following options right after `mdya`, before the command name (`mdya --data-dir ~/work-mdya search "手順"`). If you write them after the command name, it is an argument error (exit code 2).

| Option | Meaning | Default |
|---|---|---|
| `--data-dir <directory>` | The settings and index directory ([Settings: Directories](configuration.md#directories)) | `~/.mdya` |
| `--model-cache-dir <directory>` | The embedding model cache directory | `~/.mdya-models` |
| `--log-level <level>` | The log level from which logs are written ([Settings: Logs and color](configuration.md#logs-and-color)) | `warn` (`error` for one part) |
| `--log-format <format>` | The format of the logs (`compact`, `pretty`, `json`) | `compact` |
| `--no-color` | Do not color the output | Color the output only when it goes to a terminal |

### Where output goes

- Only the result of the command goes to standard output (stdout).
- Messages (that the command succeeded, progress, a summary at the end, and so on), errors, and logs go to standard error (stderr).
- Commands that accept `--json` return the result as JSON with it. Without it, they return the result in a human-readable format.
- On an error, nothing is written to stdout, even with `--json`. The error message, its cause, and how to fix it are written to stderr in a human-readable format. From scripts, tell success from failure by the exit code, not by the contents of stdout.
- In what is written in a human-readable format, control characters (including newlines and tabs) and characters that change the direction of text (U+202A to U+202E, U+2066 to U+2069) in strings that come from ingested documents or from the user (keys, snippets, locations, and so on) are replaced with a visible form. This keeps the terminal from being manipulated by what was ingested. The body of a document that `mdya get` returns is written as it is, without these replacements.

### Exit codes

| Exit code | Meaning |
|---|---|
| 0 | Success |
| 1 | Error |
| 2 | Argument error |
| 101 | Unexpected failure (a bug in mdya) |
| 137 | Stopped because memory went over the limit |

The exit code and how to fix it for each error are in [Errors and how to fix them](errors.md).

### Progress

Ingestion, rebuilding the embeddings, and maintenance write their progress to stderr. When stderr is a terminal, the progress is shown as a progress bar that is redrawn as it goes. When stderr is not a terminal (redirected to a file, piped, and so on), and when the environment variable `TERM` is `dumb` (outside Windows, also when `TERM` is not set), the state is written as one line at the start and every 10 seconds.

```
Updating 0/3, 0 chunks embedded, 10s
```

### Commands that ask for confirmation

`mdya collection remove` and `mdya remove` ask for confirmation on the terminal before removing (`[y/N]`). Answering `y` removes. With `--yes`, they remove without asking for confirmation. When you run them outside a terminal (from a script, for example), `--yes` is needed.

## Adding a collection (`mdya collection add`)

```
mdya collection add <collection name> [--type <collection type>] [--description <description>] [<location>]
```

| Argument or option | Meaning |
|---|---|
| `<collection name>` | The name of the collection to add. 1 to 64 characters, made only of ASCII letters and digits, `-`, and `_` |
| `--type <collection type>` | One of `directory` (the default), `archive`, and `raw` ([Collections and collection types](collections.md)) |
| `--description <description>` | The description of the collection. Optional |
| `<location>` | The directory for a directory collection, and the archive file for an archive collection. Not given for a raw collection |

Adding a collection does not ingest anything. Ingestion is done by `mdya update` or `mdya put`.

```
$ mdya collection add notes ~/notes --description "個人のメモ"
Created the collection "notes" (directory).
  path: /Users/me/notes
  description: 個人のメモ
$ mdya collection add notes-zip ~/notes.zip --type archive
Created the collection "notes-zip" (archive).
  path: /Users/me/notes.zip
$ mdya collection add memo --type raw
Created the collection "memo" (raw).
```

Possible errors: [A collection with the same name exists](errors.md#a-collection-with-the-same-name-exists), [The collection name does not follow the naming rules](errors.md#the-collection-name-does-not-follow-the-naming-rules), [Type-specific attributes are missing or unexpected](errors.md#type-specific-attributes-are-missing-or-unexpected), [The location was not found](errors.md#the-location-was-not-found), [The location does not fit the collection type](errors.md#the-location-does-not-fit-the-collection-type)

## Removing a collection (`mdya collection remove`)

```
mdya collection remove <collection name> [--yes] [--no-optimize]
```

Removes the collection and its documents from the index. The source (the directory or the archive file) is not touched. Asks for confirmation before removing. For `--no-optimize`, see [Maintaining the index](#maintaining-the-index-mdya-optimize).

```
$ mdya collection remove notes-zip --yes
Removed the collection "notes-zip".
```

The documents of a raw collection cannot be ingested again by an update. To bring them back after removing the collection, you need to pass the same document content again with `mdya put`.

Possible errors: [The collection does not exist](errors.md#the-collection-does-not-exist), [No confirmation](errors.md#no-confirmation)

## Listing collections (`mdya collection list`)

```
mdya collection list [--json]
```

For each collection, returns the collection name, the collection type, the number of documents, the description, and the type-specific attributes. Collections are listed in order of name (in order of the Unicode code points of the characters).

```
$ mdya collection list
notes (directory, 0 documents)
  個人のメモ
  path: /Users/me/notes
```

With `--json`, the output looks like this. `description` is `null` when there is no description. `attributes` holds the location (`path`) for directory and archive collections, and is an empty object for raw collections.

```json
{"collections":[{"name":"notes","type":"directory","description":"個人のメモ","document_count":0,"attributes":{"path":"/Users/me/notes"}}]}
```

## Changing a collection's location or description (`mdya collection set`)

```
mdya collection set <collection name> [--description <description> | --no-description] [<location>]
```

Changes the description, or the location to ingest from. Give at least one of them. `--no-description` removes the description. The new location is used at the next update. This command does not change the documents.

The collection name and the collection type cannot be changed. To change them, remove the collection and add it again.

```
$ mdya collection set notes --description "仕事のメモ"
Changed the collection "notes".
  path: /Users/me/notes
  description: 仕事のメモ
```

Possible errors: [The collection does not exist](errors.md#the-collection-does-not-exist), [Nothing to change was given](errors.md#nothing-to-change-was-given), [Type-specific attributes are missing or unexpected](errors.md#type-specific-attributes-are-missing-or-unexpected), [The location was not found](errors.md#the-location-was-not-found), [The location does not fit the collection type](errors.md#the-location-does-not-fit-the-collection-type), [A collection has a type this mdya does not know](errors.md#a-collection-has-a-type-this-mdya-does-not-know)

## Updating collections (`mdya update`)

```
mdya update <collection name> [--force] [--no-optimize]
mdya update --all [--force] [--no-optimize]
```

Brings directory and archive collections up to date with the current contents of their sources ([Collections and collection types](collections.md)). `mdya update <collection name>` updates the one collection you specify, and `mdya update --all` updates all the directory and archive collections. Raw collections cannot be updated.

| Option | Meaning |
|---|---|
| `--force` | Ingest documents again even if they have not changed |
| `--no-optimize` | Do not maintain the index at the end ([Maintaining the index](#maintaining-the-index-mdya-optimize)) |

Documents that have not changed are skipped, so from the second time on, only the documents that have changed are ingested. The first time an embedding model is used, mdya fetches the embedding model ([Settings: Embedding model](configuration.md#embedding-model)).

The progress shows the number of documents processed and the total number of documents to process. The progress bar does not show the number of chunks; it shows a mark at the head of its line instead. The mark turns as more chunks have their embeddings made. While one document takes long to process, a turning mark shows that it is going on. When the state is written as one line, the line also shows the number of chunks whose embeddings have been made. At the end, the number of documents for each result is shown.

```
$ mdya update notes
Updated the collection "notes": 3 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed.
$ mdya update --all
Updated 2 collections: 4 added, 0 changed, 2 unchanged, 0 re-ingested, 0 deleted, 0 failed.
```

| Result | Meaning |
|---|---|
| `added` | Newly added |
| `changed` | Ingested again because it has changed |
| `unchanged` | Skipped because it has not changed |
| `re-ingested` | Not changed, but ingested again because of `--force` |
| `deleted` | Removed because it is no longer in the source |
| `failed` | Could not be read ([Some documents cannot be read](errors.md#some-documents-cannot-be-read)) |

When there are unreadable documents, or sources that cannot be read, mdya ingests the other documents and collections first, and then ends with exit code 1.

When it stops on an error, too, it shows, after the error message, the number of documents for each result up to where it stopped. The documents written before it stopped are kept, so you can tell how far it got.

If you stop it partway (with Ctrl+C, for example), run the same command again. Unless you gave `--force`, documents that have not changed are skipped, so it carries on from where it stopped.

Possible errors: [The collection does not exist](errors.md#the-collection-does-not-exist), [A collection of a type that cannot be updated was given](errors.md#a-collection-of-a-type-that-cannot-be-updated-was-given), [Some documents cannot be read](errors.md#some-documents-cannot-be-read), [A source cannot be read](errors.md#a-source-cannot-be-read), [The embedding model cannot be used](errors.md#the-embedding-model-cannot-be-used), [The embeddings have not been rebuilt](errors.md#the-embeddings-have-not-been-rebuilt), [Some chunks are longer than the embedding model's context length](errors.md#some-chunks-are-longer-than-the-embedding-models-context-length), [A collection has a type this mdya does not know](errors.md#a-collection-has-a-type-this-mdya-does-not-know)

## Ingesting a document you pass in (`mdya put`)

```
mdya put <collection name> <key> --file-type <file type> [--force] [--no-optimize] <location>
```

Ingests one document into a raw collection. If there is no document with that key, it is added; if there is one, it is replaced.

| Argument or option | Meaning |
|---|---|
| `<collection name>` | The name of a raw collection |
| `<key>` | The key of the document. Any string is accepted, including the empty string |
| `--file-type <file type>` | For Markdown, one of `text/markdown`, `markdown`, and `md`. For PDF, one of `application/pdf` and `pdf`. Upper and lower case are not distinguished. When you add a `charset` to `text/markdown`, only `UTF-8` is accepted |
| `--force` | Ingest the document again even if its body is the same as before |
| `--no-optimize` | Do not maintain the index at the end ([Maintaining the index](#maintaining-the-index-mdya-optimize)) |
| `<location>` | The file to read the document content from. With `-`, it is read from standard input |

```
$ mdya put memo idea.md --file-type md ~/memo-idea.md
Added "idea.md" to the collection "memo".
$ printf '# 買うもの\n\n- 牛乳\n' | mdya put memo shopping.md --file-type text/markdown -
Added "shopping.md" to the collection "memo".
$ mdya put memo idea.md --file-type md ~/memo-idea.md
Skipped "idea.md" in the collection "memo": it has not changed.
```

Possible errors: [The collection does not exist](errors.md#the-collection-does-not-exist), [A collection of a type that does not take document content was given](errors.md#a-collection-of-a-type-that-does-not-take-document-content-was-given), [The file type is not supported](errors.md#the-file-type-is-not-supported), [Some documents cannot be read](errors.md#some-documents-cannot-be-read), [The embedding model cannot be used](errors.md#the-embedding-model-cannot-be-used), [The embeddings have not been rebuilt](errors.md#the-embeddings-have-not-been-rebuilt), [Some chunks are longer than the embedding model's context length](errors.md#some-chunks-are-longer-than-the-embedding-models-context-length), [A collection has a type this mdya does not know](errors.md#a-collection-has-a-type-this-mdya-does-not-know)

## Removing a document (`mdya remove`)

```
mdya remove <collection name> <key> [--yes] [--no-optimize]
```

Removes one document from the index. The source is not touched. Asks for confirmation before removing.

If a document of a directory or archive collection is still in the source, it is ingested again at the next update.

```
$ mdya remove memo shopping.md --yes
Removed "shopping.md" from the collection "memo".
$ mdya remove notes setup.md --yes
Removed "setup.md" from the collection "notes".
If it is still in the collection's source, it comes back at the next update.
```

Possible errors: [The collection does not exist](errors.md#the-collection-does-not-exist), [The document does not exist](errors.md#the-document-does-not-exist), [No confirmation](errors.md#no-confirmation), [A collection has a type this mdya does not know](errors.md#a-collection-has-a-type-this-mdya-does-not-know)

## Searching (`mdya search`)

```
mdya search <query> [--mode <fts|vector|hybrid>] [-c, --collection <collection name>]... [-n, --limit <limit>] [--unit <document|chunk>] [--json]
```

| Argument or option | Meaning | Default |
|---|---|---|
| `<query>` | What to search for. A query made only of whitespace is not accepted | None (required) |
| `--mode` | The search mode: `fts` (full-text search), `vector` (vector search), or `hybrid` (hybrid search) | `hybrid` |
| `-c`, `--collection` | The collection to search. You can give several, separated with commas (`-c notes,work`) or by repeating the option (`-c notes -c work`). The documents in any of the given collections are searched. Words separated with spaces, as in `-c notes work`, are not read as collection names | All collections |
| `-n`, `--limit` | The most hits to return. From 1 to 9007199254740991 | 20 |
| `--unit` | `document` (one hit for each document) or `chunk` (one hit for each chunk) | `document` |
| `--json` | Return the result as JSON | |

### Search modes

- `fts` (full-text search): Searches by whether the words of the query appear in documents or chunks. Does not use the embedding model. The score is the BM25 value (a value computed from how many times the words appear and how rare those words are); the larger it is, the higher the hit is placed.
- `vector` (vector search): Searches by how close the query and the chunks are in meaning. The score is the cosine similarity, a value from 0 to 1. It searches with an index built for fast search, so it may miss the closest documents or chunks.
- `hybrid` (hybrid search): Combines the results of full-text search and vector search by the rank in each (RRF). The score is 1 / (60 + r), where r is the rank in a search (counted from 0), computed for both searches and added together. When something is not a hit in one of the searches, the value for that side is 0.

The meaning of the score differs by search mode, so do not compare scores from different search modes.

Full-text search splits text into words as follows.

- Text is split into words with a Japanese dictionary (IPADIC). A katakana compound word is split into its words if it is made only of words in the dictionary ("モデルキャッシュ" becomes "モデル" and "キャッシュ").
- A run of mixed letters and digits is split where it switches between letters and digits (`u32` becomes `u` and `32`). Symbols inside a word of letters also become words (the `++` of `C++`).
- Half-width and full-width forms of kana, letters, and digits, circled numbers, and the like are treated as the same characters. Upper and lower case, the presence or absence of accent marks, and differences in English word forms (`run` and `running`) are not distinguished. Differences in Japanese conjugation ("走る" and "走った") are distinguished.
- Japanese particles, auxiliary verbs, conjunctions, punctuation marks, brackets, and the like, and common English words (`the`, `a`, `you`, and so on) are not used as search words. Because of this, the negative "ない" is not used as a search word either, and "削除しない" is searched the same as "削除し". When all the words of the query are words that are not used as search words, there are no hits.
- Words of 255 bytes or more (counted in UTF-8; for Japanese, about 85 characters or more) are not taken as words.

### Results

Hits are returned from the highest score down. Hits with the same score are ordered by collection name, then by key (both in order of the Unicode code points of the characters), and, with `--unit chunk`, then by chunk index, smallest first.

- `--unit document`: Returns the documents that were hit, up to the limit.
- `--unit chunk`: Returns the chunks that were hit, each as a separate hit, up to the limit.

The snippet is a string of up to 60 characters taken from the chunk that was hit. It is taken starting 30 characters before the first of the query's words to appear in the chunk. If none of the words appears, it is taken from the start of the chunk. Newlines are replaced with spaces, and `…` is added where the start or the end was cut off.

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

The first line of each hit is the collection name and the key. The second line is the score, the index of the chunk the snippet was taken from (counted from 0), and the number of chunks in the document. The third line is the snippet.

With `--json`, the output looks like this (line breaks have been added for readability).

```json
{
  "query": "リリースの手順",
  "mode": "hybrid",
  "unit": "document",
  "collections": [],
  "limit": 20,
  "hit_count": 3,
  "hits": [
    {"collection":"notes","key":"meetings/2026-11-10.md","score":0.03306011110544205,"snippet":"## 次にやること - リリースの手順を見直す。 - 会議室の予約を、来月の分まで取る。 ","chunk_index":2,"chunk_count":3},
    {"collection":"notes","key":"release.md","score":0.03306011110544205,"snippet":"# リリースの手順 タグを打つ前に、変更履歴を書き、バージョン番号を上げます。 ","chunk_index":0,"chunk_count":3},
    {"collection":"notes","key":"setup.md","score":0.032258063554763794,"snippet":"# 開発環境の用意 新しい PC で開発を始めるときの手順です。 1. リポジトリを取得します。 2. 必要な道具を…","chunk_index":0,"chunk_count":1}
  ],
  "warnings": []
}
```

- `query`, `mode`, `unit`, `collections`, `limit`: The query you gave, the search mode, `document` or `chunk`, the collection names, and the limit. When no collection was specified, `collections` is an empty array.
- `hit_count`: The number of hits returned.
- In each hit, `chunk_index` is the index of the chunk the snippet was taken from, and `chunk_count` is the number of chunks in the document. You can use them to specify a range of chunks with `mdya get`.
- `warnings`: An array of warning texts. For example, a warning goes in when the configured embedding model differs from the embedding model the index's embeddings were made with ([Some embeddings were made with another model than the configured one](errors.md#some-embeddings-were-made-with-another-model-than-the-configured-one)).

Possible errors: [The query is empty](errors.md#the-query-is-empty), [The limit is zero](errors.md#the-limit-is-zero), [The query is longer than the embedding model's context length](errors.md#the-query-is-longer-than-the-embedding-models-context-length), [The collection does not exist](errors.md#the-collection-does-not-exist), [The embedding model cannot be used](errors.md#the-embedding-model-cannot-be-used), [Some embeddings were made with another model than the configured one](errors.md#some-embeddings-were-made-with-another-model-than-the-configured-one)

## Getting a document (`mdya get`)

```
mdya get <collection name> <key> [--chunk-index <index> [--end-chunk-index <index>]] [--no-size-limit]
```

Writes the body of the document to stdout without changing it. No newline is added at the end either. What it returns is the body of the document in the index; the source is not read again.

| Option | Meaning |
|---|---|
| `--chunk-index <index>` | Return the characters in the range of the chunk with that index, instead of the whole body of the document |
| `--end-chunk-index <index>` | Return the characters in the range from the chunk of `--chunk-index` to the chunk with this index (this index included), as one continuous piece. Use an index equal to or greater than that of `--chunk-index`. If the index is past the last chunk, it returns up to the end of the body of the document |
| `--no-size-limit` | Do not apply the output size limit ([Settings: Output size limit for getting documents](configuration.md#output-size-limit-for-getting-documents)) |

The ranges of the chunks cover the body of the document without gaps. The ranges of adjacent chunks may overlap. When you specify a range in one command, the overlapping characters are included only once.

```
$ mdya get notes release.md --chunk-index 0
# リリースの手順

タグを打つ前に、変更履歴を書き、バージョン番号を上げます。

```

Possible errors: [The collection does not exist](errors.md#the-collection-does-not-exist), [The document does not exist](errors.md#the-document-does-not-exist), [The chunk does not exist](errors.md#the-chunk-does-not-exist), [The chunk range is not valid](errors.md#the-chunk-range-is-not-valid), [The output is too large](errors.md#the-output-is-too-large)

## Showing the state of the index (`mdya status`)

```
mdya status [--json]
```

```
$ mdya status
version: mdya X.Y.Z
configured embedding model: cl-nagoya/ruri-v3-30m (256 dimensions)
index embedding model: cl-nagoya/ruri-v3-30m (256 dimensions, the configured one)
  chunks with embeddings: 7 (0 truncated)
chunks without embeddings: 0
collections: 1
documents: 3
chunks: 7
```

- `configured embedding model`: The configured embedding model, and the number of dimensions of the embeddings it makes.
- `index embedding model`: The embedding model the index's embeddings were made with, and whether it is the same as the configured embedding model. The line below it shows the number of chunks that have embeddings made with that embedding model, and how many of those chunks have embeddings that do not represent the whole chunk (`truncated`; [Some chunks are longer than the embedding model's context length](errors.md#some-chunks-are-longer-than-the-embedding-models-context-length)). When the index has no chunks, `index embedding model: none` is shown.
- `chunks without embeddings`: The number of chunks that have no embeddings.
- `collections`, `documents`, `chunks`: The numbers of collections, documents, and chunks in the whole index.
- `warning`: A warning in the result. When the configured embedding model could not be used and its number of dimensions could not be checked, a last line shows that, with the reason ([The embedding model cannot be used](errors.md#the-embedding-model-cannot-be-used)). With `--json`, it is in `warnings`.

With `--json`, the output looks like this.

```json
{"version":"X.Y.Z","configured_embedding_model":"cl-nagoya/ruri-v3-30m","configured_embedding_dimension":256,"index_embedding_models":[{"model":"cl-nagoya/ruri-v3-30m","dimension":256,"configured":true,"chunk_count":7,"truncated_chunk_count":0}],"chunks_without_embedding_count":0,"collection_count":1,"document_count":3,"chunk_count":7,"warnings":[]}
```

`configured_embedding_dimension` and `configured` are `null` when the configured embedding model could not be used and its number of dimensions could not be checked. In that case, the reason is put in `warnings` ([The embedding model cannot be used](errors.md#the-embedding-model-cannot-be-used)). It is not treated as an error.

## Showing the settings (`mdya config list`)

```
mdya config list [--json]
```

Returns the names of the settings and their current values. For settings you have not changed, the default values are returned. What each setting means is in [Settings](configuration.md).

```
$ mdya config list
mcp.show_version = true
embedding.model = cl-nagoya/ruri-v3-30m
embedding.ollama_endpoint = http://127.0.0.1:11434
resources.memory_limit_mb = 8192
resources.embedding_parallelism = 8
get.cli_limit_bytes = 1048576
get.mcp_limit_bytes = 1048576
```

With `--json`, returns a JSON object whose member names are the names of the settings.

```json
{"mcp.show_version":true,"embedding.model":"cl-nagoya/ruri-v3-30m","embedding.ollama_endpoint":"http://127.0.0.1:11434","resources.memory_limit_mb":8192,"resources.embedding_parallelism":8,"get.cli_limit_bytes":1048576,"get.mcp_limit_bytes":1048576}
```

## Changing settings (`mdya config set`, `mdya config reset`)

```
mdya config set <name> <value>
mdya config reset <name>
```

`mdya config set` changes one setting, and `mdya config reset` puts it back to its default value. The names of the settings and the values they accept are in [Settings](configuration.md). Do not edit the settings file directly. How mdya behaves when the file has been edited directly is not guaranteed.

```
$ mdya config set resources.embedding_parallelism 4
Changed resources.embedding_parallelism from 8 to 4.
$ mdya config reset resources.embedding_parallelism
Changed resources.embedding_parallelism from 4 to 8.
```

When you change the embedding model, mdya tells you if the embedding model the index's embeddings were made with differs from the changed setting. Until you rebuild them with `mdya embedding rebuild`, vector search finds nothing, and `mdya update` and `mdya put` fail ([Settings: Changing the embedding model](configuration.md#changing-the-embedding-model)).

```
$ mdya config set embedding.model sentence-transformers/all-MiniLM-L6-v2
Changed embedding.model from cl-nagoya/ruri-v3-30m to sentence-transformers/all-MiniLM-L6-v2.
The index's embeddings were made with another model (cl-nagoya/ruri-v3-30m).
Until you run mdya embedding rebuild, vector search finds nothing, and update and put fail.
```

When the name of the new embedding model is the same as that of the embedding model the index's embeddings were made with, and that model cannot be used so its number of dimensions cannot be checked, the setting is changed without an error. mdya tells you that it cannot tell whether the index's embeddings were made with the new embedding model, and why ([The embedding model cannot be used](errors.md#the-embedding-model-cannot-be-used)).

For an MCP server that is running, the change takes effect when the server is restarted.

Possible errors: [There is no setting with that name](errors.md#there-is-no-setting-with-that-name), [The value is not valid for the setting](errors.md#the-value-is-not-valid-for-the-setting)

## Rebuilding the embeddings (`mdya embedding rebuild`)

```
mdya embedding rebuild [--force] [--no-optimize]
```

Rebuilds the index's embeddings with the configured embedding model. The sources are not read, and the bodies of the documents and the chunks are not changed.

- When the embedding model the index's embeddings were made with is the same as the configured one, it does nothing.
- With `--force`, it rebuilds them even when they are the same. Use it when you have replaced an Ollama model with another model that keeps the same name and whose embeddings also have the same number of dimensions. mdya tells embedding models apart by their name and the number of dimensions of their embeddings, so it cannot tell that the model was replaced. When you replace it with a model with a different number of dimensions, the embeddings are rebuilt even without `--force`.
- While rebuilding, mdya keeps the previous embeddings, and switches to the new embeddings when it has finished rebuilding all the chunks. In a rebuild after changing the embedding model, the previous embeddings do not match the setting, so until the switch, vector search finds nothing in searches from the CLI and in MCP servers started after the setting was changed. An MCP server started before the setting was changed compares with the setting as it was when the server started, so it finds hits until the switch, and finds none after it ([The MCP server: When the settings change](mcp.md#when-the-settings-change)). When you rebuild with `--force` and the same embedding model, search uses the previous embeddings until the switch.
- If it is stopped partway, the half-made embeddings are not used for search. Running the same command again finishes the rebuild.

```
$ mdya embedding rebuild
Rebuilt the embeddings of 15 chunks.
$ mdya embedding rebuild
Nothing was rebuilt: the embeddings were already made with the configured model.
```

Possible errors: [The embedding model cannot be used](errors.md#the-embedding-model-cannot-be-used), [Some chunks are longer than the embedding model's context length](errors.md#some-chunks-are-longer-than-the-embedding-models-context-length)

## Maintaining the index (`mdya optimize`)

```
mdya optimize
```

Each time the index is written to, it keeps the state from before the write, and small files also pile up. Maintenance combines the small files to keep search fast, and, of the states from before writes, removes the ones made more than 7 days ago, to keep disk usage down.

The following commands also maintain the index at the end by default. With `--no-optimize`, they do not maintain it. In that case, you can maintain it later with `mdya optimize`.

- `mdya collection remove`
- `mdya update`
- `mdya put`
- `mdya remove`
- `mdya embedding rebuild`

```
$ mdya optimize
Maintained the index.
```

When an index that has not been written to for more than 7 days is maintained, a search that another process (such as an MCP server) started before the maintenance may fail with an error ([The index or settings cannot be read](errors.md#the-index-or-settings-cannot-be-read)). If you search again, the index can be read. A write with `--no-optimize` spends no time on maintenance, and this error does not occur either.

When the maintenance at the end of a write command fails, the command's work is left completed, and the failure of the maintenance is shown as a warning. A failed maintenance does not change the exit code. When maintenance fails in `mdya optimize`, it ends with an error ([The index or settings cannot be written](errors.md#the-index-or-settings-cannot-be-written)).

## Starting the MCP server (`mdya mcp`)

```
mdya mcp [--http] [--address <address>]
```

Starts the MCP server, which MCP clients such as AI assistants use. How to use it is described in [The MCP server](mcp.md).

| Option | Meaning | Default |
|---|---|---|
| `--http` | Listen over HTTP instead of stdio | stdio |
| `--address <address>` | The address to listen on over HTTP, in the form `<host>:<port>`. Put an IPv6 address in `[ ]` | `127.0.0.1:8000` |

Possible errors: [The address to listen on cannot be used](errors.md#the-address-to-listen-on-cannot-be-used), [The connection with the MCP client cannot be started](errors.md#the-connection-with-the-mcp-client-cannot-be-started)

## Showing the mdya version (`mdya --version`, `mdya -v`)

```
$ mdya --version
mdya X.Y.Z
```

`mdya -v` is the same as `mdya --version`.

## Showing help (`--help`, `-h`)

You can add `--help` or `-h` to any command. `mdya --help` writes the list of commands to stdout, `mdya collection --help` the list of the commands under `collection`, and `mdya collection add --help` the arguments of that command and their descriptions.

When `--help` is given, help is shown without checking the other arguments (`mdya search --help` shows help even without a query). When the command name is not valid (`mdya nosuchcmd --help`), it is an argument error (exit code 2).

`mdya` with no arguments is the same as `mdya --help`. A command given without the command under it (`mdya collection`) does not show help; it is an argument error (exit code 2).
