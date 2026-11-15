# Errors and how to fix them

Use this page to look up what happened and how to fix it when mdya ends with an error.

On an error, mdya writes to stderr an error message that starts with `Error:`, its cause (`Caused by:`), and how to fix it (`How to fix:`). For an error from an MCP tool, the same explanation goes in `message` ([The MCP server: Errors](mcp.md#errors)).

```
Error: the collection "nosuch" does not exist

How to fix:
    Check the collection name in the list of collections (mdya collection list, or the list_collections tool).
```

## Exit codes

| Exit code | Meaning |
|---|---|
| 0 | Success |
| 1 | An error. Use the error message to find the section in the table below |
| 2 | An argument error: an error that can be told from the given arguments alone |
| 101 | [Unexpected failure](#unexpected-failure) |
| 137 | [Memory went over the limit](#memory-went-over-the-limit) |
| A value set by the OS | [Interruption](#interruption) (Ctrl+C and the like) |

## Finding an error by its message

| Error message (after `Error:`) | Exit code | Section |
|---|---|---|
| `a collection named "…" already exists` | 1 | [A collection with the same name exists](#a-collection-with-the-same-name-exists) |
| `the collection name "…" does not follow the naming rules` | 2 | [The collection name does not follow the naming rules](#the-collection-name-does-not-follow-the-naming-rules) |
| `the type-specific attributes are missing or unexpected …` | 2, 1 | [Type-specific attributes are missing or unexpected](#type-specific-attributes-are-missing-or-unexpected) |
| `the location was not found` | 1 | [The location was not found](#the-location-was-not-found) |
| `the location does not fit the collection type` | 1 | [The location does not fit the collection type](#the-location-does-not-fit-the-collection-type) |
| `the collection "…" does not exist` | 1 | [The collection does not exist](#the-collection-does-not-exist) |
| `nothing to change was given` | 2 | [Nothing to change was given](#nothing-to-change-was-given) |
| `the collection "…" is of a type that cannot be updated` | 1 | [A collection of a type that cannot be updated was given](#a-collection-of-a-type-that-cannot-be-updated-was-given) |
| `the collection "…" is of a type that does not take document content` | 1 | [A collection of a type that does not take document content was given](#a-collection-of-a-type-that-does-not-take-document-content-was-given) |
| `the file type "…" is not supported` | 2 | [The file type is not supported](#the-file-type-is-not-supported) |
| `the document "…" does not exist in the collection "…"` | 1 | [The document does not exist](#the-document-does-not-exist) |
| `the query is empty` | 2 | [The query is empty](#the-query-is-empty) |
| `the limit is zero` | 2 | [The limit is zero](#the-limit-is-zero) |
| `the chunk does not exist` | 1 | [The chunk does not exist](#the-chunk-does-not-exist) |
| `the chunk range is not valid` | 2 | [The chunk range is not valid](#the-chunk-range-is-not-valid) |
| `there is no setting named "…"` | 2 | [There is no setting with that name](#there-is-no-setting-with-that-name) |
| `the value is not valid for the setting …` | 2 | [The value is not valid for the setting](#the-value-is-not-valid-for-the-setting) |
| `a confirmation is needed, but there is no terminal to ask in and --yes was not given` | 2 | [No confirmation](#no-confirmation) |
| `the document's body is too large to return: …` (also `the chunk is …`, `the chunk range is …`) | 1 | [The output is too large](#the-output-is-too-large) |
| `the document content cannot be read`, `the document's body cannot be extracted` | 1 | [Some documents cannot be read](#some-documents-cannot-be-read) |
| `the embedding model cannot be used` | 1 | [The embedding model cannot be used](#the-embedding-model-cannot-be-used) |
| `the configured embedding model … differs from the one the index's embeddings were made with, …` | 1 | [The embeddings have not been rebuilt](#the-embeddings-have-not-been-rebuilt) |
| `a rebuild of the embeddings was stopped while switching over to the new embedding model` | 1 | [The embeddings have not been rebuilt](#the-embeddings-have-not-been-rebuilt) |
| `the query is longer than the embedding model's context length` | 1 | [The query is longer than the embedding model's context length](#the-query-is-longer-than-the-embedding-models-context-length) |
| `the index cannot be read`, `the settings cannot be read`, `the collection information cannot be read` | 1 | [The index or settings cannot be read](#the-index-or-settings-cannot-be-read) |
| `the index cannot be written`, `the settings cannot be written`, `the collection information cannot be written`, `the write lock cannot be taken` | 1 | [The index or settings cannot be written](#the-index-or-settings-cannot-be-written) |
| `another write operation is in progress` | 1 | [Another write operation is in progress](#another-write-operation-is-in-progress) |
| `mdya's memory usage went over the limit of … MB` | 137 | [Memory went over the limit](#memory-went-over-the-limit) |
| `cannot listen on …` | 1 | [The address to listen on cannot be used](#the-address-to-listen-on-cannot-be-used) |
| `the connection with the MCP client cannot be started` | 1 | [The connection with the MCP client cannot be started](#the-connection-with-the-mcp-client-cannot-be-started) |
| `the settings and index directory cannot be determined` | 1 | [The settings and index directory cannot be determined](#the-settings-and-index-directory-cannot-be-determined) |
| `this version of mdya does not know the collection type "…" of the collection "…"` | 1 | [A collection has a type this mdya does not know](#a-collection-has-a-type-this-mdya-does-not-know) |
| `an unexpected failure occurred, which is a bug in mdya` | 101 | [Unexpected failure](#unexpected-failure) |
| `the arguments are not valid: …` | 2 | [Argument errors](#argument-errors) |
| `the tool input … is not valid: …` | (MCP only) | `invalid_input` in [The MCP server: Errors](mcp.md#errors) |

When there is no `Error:` line and an update ends with exit code 1, the cause is [Some documents cannot be read](#some-documents-cannot-be-read) or [A source cannot be read](#a-source-cannot-be-read). Look at the warnings (`WARN`) on stderr.

## Invalid input

These are errors where something you gave is not valid. In every case, mdya ends without doing anything, and nothing in mdya changes.

### A collection with the same name exists

- When it happens: In `mdya collection add`, when a collection with the same name already exists. Exit code 1.
- How to fix: Use a different collection name.

### The collection name does not follow the naming rules

- When it happens: In `mdya collection add`, when the collection name does not follow the rules (made only of ASCII letters and digits, `-`, and `_`, 1 to 64 characters long). Exit code 2.
- How to fix: Use a collection name that follows the rules.

### Type-specific attributes are missing or unexpected

- When it happens: When you do not give a location for a directory or archive collection, or when you give a location for a raw collection. Exit code 2 in `mdya collection add`, and exit code 1 in `mdya collection set` (because this cannot be known without looking at the type of the existing collection).
- How to fix: Give a location for directory and archive collections, and do not give one for raw collections.

### The location was not found

- When it happens: In `mdya collection add` and `mdya collection set`, when the given location does not exist. This also covers the case where mdya cannot read whether the location is a directory or a file, for example because of missing permissions, and the case where the location cannot be made into an absolute path (`~` when the home directory is unknown, or a relative path when the working directory cannot be read). The cause (`Caused by:`) shows why it could not be read. Exit code 1.
- How to fix: Give a location that exists. If it could not be read, fix the permissions so that mdya can read it, or give a location that can be read. If it could not be made into an absolute path, give an absolute path, or move to a working directory that can be read and try again.

### The location does not fit the collection type

- When it happens: When you give a file for a directory collection, or when you give a directory, or a file in a format it does not handle, for an archive collection. Exit code 1.
- How to fix: Give a directory for a directory collection, and a `.zip`, `.tar`, `.tar.gz`, or `.tgz` file for an archive collection.

### The collection does not exist

- When it happens: In commands and tools where you specify a collection, when there is no collection with that name. Exit code 1. The MCP `code` is `collection_not_found`.
- How to fix: Check the collection name with `mdya collection list` (`list_collections` in MCP).

### Nothing to change was given

- When it happens: When you give `mdya collection set` neither a description nor a location. Exit code 2.
- How to fix: Give `--description`, `--no-description`, or a location.

### A collection of a type that cannot be updated was given

- When it happens: When you give `mdya update` a raw collection. Exit code 1.
- How to fix: Change the documents of a raw collection with `mdya put`.

### A collection of a type that does not take document content was given

- When it happens: When you give `mdya put` a directory or archive collection. Exit code 1.
- How to fix: For the documents of directory and archive collections, change the source, then ingest them with `mdya update`.

### The file type is not supported

- When it happens: When you give `--file-type` of `mdya put` a type other than Markdown or PDF. Exit code 2.
- How to fix: Give Markdown as `text/markdown` (`markdown`, `md`), and PDF as `application/pdf` (`pdf`).

### The document does not exist

- When it happens: In `mdya remove`, `mdya get`, and the MCP `get_document`, when there is no document with that key. Exit code 1. The MCP `code` is `document_not_found`.
- How to fix: Check the key, for example against the key in the search results.

### The query is empty

- When it happens: In a search, when the query is only whitespace. Exit code 2. The MCP `code` is `invalid_input`.
- How to fix: Give a query that is not only whitespace.

### The limit is zero

- When it happens: In a search, when you give 0 as the limit. Exit code 2. The MCP `code` is `invalid_input`.
- How to fix: Give a limit of 1 or more.

### The chunk does not exist

- When it happens: When getting a document, when the start chunk index is equal to or greater than the number of chunks in that document. Exit code 1. The MCP `code` is `invalid_input`.
- How to fix: As the start, give an index smaller than the number of chunks in the document (`chunk_count` in the search results).

### The chunk range is not valid

- When it happens: When getting a document, when you give an end chunk index without a start chunk index, or when the end is smaller than the start. Exit code 2. The MCP `code` is `invalid_input`.
- How to fix: Give the start, and make the end equal to or greater than the start.

### There is no setting with that name

- When it happens: In `mdya config set` and `mdya config reset`, when there is no setting with that name. Exit code 2.
- How to fix: Check the setting names with `mdya config list`.

### The value is not valid for the setting

- When it happens: In `mdya config set`, when you give a value that the setting does not accept. Exit code 2.
- How to fix: Use a value that the setting accepts ([Settings](configuration.md#changing-settings)). The how-to-fix part (`How to fix:`) also shows the values it accepts.

## No confirmation

- When it happens: In `mdya collection remove` and `mdya remove`, when you do not answer the confirmation with `y` (exit code 1, and `Did not remove …` is shown), or when there is no terminal and you do not add `--yes` (exit code 2).
- What remains: mdya removes nothing.
- How to fix: Answer the confirmation in a terminal, or add `--yes`.

## The output is too large

- When it happens: When getting a document, when the size of the text to return goes over the output size limit ([Settings](configuration.md#output-size-limit-for-getting-documents)). Exit code 1. The MCP `code` is `output_too_large`.
- What remains: mdya returns nothing.
- How to fix: Give a chunk range and get the document in parts. On the CLI, you can also remove the limit with `--no-size-limit` and get the document.

```
Error: the document's body is too large to return: 413 bytes, over the limit of 10 bytes

How to fix:
    Get the document in parts by giving a range of chunks.
    On the command line, you can also remove the limit with --no-size-limit.
```

## Some documents cannot be read

- When it happens: During ingestion, when the document content cannot be read or the document's body cannot be extracted. For example, there is no permission to read the file, the PDF is broken, the content of a Markdown document is not UTF-8, the relative path of the file cannot be read as UTF-8, or an entry in an archive is one of the entries that cannot be read ([Collections and collection types: Entries that become unreadable documents](collections.md#entries-that-become-unreadable-documents)).
- On update: mdya skips the unreadable documents, ingests the other documents, and ends with exit code 1. For each document it could not read, it writes the key and the reason as a warning. The summary at the end counts them as `failed`. For a document that could not be read, the document version ingested before, if there is one, stays as it is.
- On `mdya put`: mdya does nothing and ends with exit code 1.

```
WARN mdya::ingest::flow: a document cannot be read and was skipped collection="broken" key="bad.md" cause="the document's body cannot be extracted: invalid utf-8 sequence of 1 bytes from index 0"
Updated the collection "broken": 1 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 1 failed.
```

- How to fix: Make the document readable (fix the permissions, use a file that is not broken, make the file and directory names UTF-8, re-create the archive so that none of its entries is one that cannot be read, and so on), then ingest it again.

Warnings are logs, so they are not shown with `--log-level error`.

## A source cannot be read

- When it happens: During an update, when the collection's source itself cannot be read. For a directory collection, when the directory does not exist or there is no permission to read it. For an archive collection, when the archive as a whole cannot be read ([Collections and collection types: When the whole archive cannot be read](collections.md#when-the-whole-archive-cannot-be-read)). When the directory exists and is empty, this does not apply, and all the documents of that collection are deleted.
- Behavior: mdya skips that collection, and with `mdya update --all`, it goes on with the other collections. Finally, it ends with exit code 1. For each collection it could not read, it writes the collection name and the reason as a warning.
- What remains: The documents of a collection that could not be read stay in the index as they are.

```
WARN mdya::archive_ingest: the source cannot be read and the collection was not updated collection="broken-zip" cause="no valid end of central directory record"
Updated the collection "broken-zip": 0 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed.
```

- How to fix: Make the source readable (connect the disk, fix the location, and so on), then run the update again. If the location of the source has changed, change the location with `mdya collection set`.

For a directory collection, when the directory itself can be read but only part of its contents cannot (a subdirectory has no read permission, an external disk was disconnected partway through, and so on), the following applies.

- Files that could be read are ingested. mdya deletes only documents it has confirmed do not exist. Documents that were in a directory that could not be read are not deleted. When mdya cannot tell which parts could not be read, it deletes no documents at all in that collection.
- For each part it could not read, mdya writes the location and the reason as a warning, and finally ends with exit code 1.
- How to fix: Make the parts that could not be read readable, then run the update again.

## The embedding model cannot be used

- When it happens: When the embedding model needs to be used but cannot be. For example, the network cannot be reached when mdya tries to get a model used for the first time, `HF_TOKEN` is missing for a gated model, the embedding model cache directory cannot be determined, or the embeddings are left to Ollama but Ollama is not running. The cause (`Caused by:`) shows why it cannot be used.
- When making embeddings (vector search, hybrid search, update, `mdya put`, `mdya embedding rebuild`): mdya ends with exit code 1. The MCP `code` is `embedding_model_unavailable`. If the model becomes unusable partway through an update, mdya stops there and keeps the documents ingested up to that point. Full-text search and getting documents keep working.
- When only checking the dimensions of the embeddings (`mdya status`, changing the embedding model setting, and an update or `mdya put` that had no documents to make embeddings for): mdya does not treat it as an error and goes on. It reports that the dimensions could not be checked, and why: in the result's warnings for `mdya status`, in the messages when changing the setting, and as warnings for update and `mdya put`. However, when the index has no embeddings at all, update and `mdya put` may end with exit code 1, the same as when making embeddings, even when there are no documents to make embeddings for.
- How to fix: Remove the reason (connect to the network, set `HF_TOKEN`, give the absolute path of the cache directory with `--model-cache-dir`, start Ollama, and so on), then do the operation again. For the MCP server, to give `--model-cache-dir` again, restart the server.

## Some embeddings were made with another model than the configured one

- When it happens: In vector search and hybrid search, when the embedding model that made the index's embeddings differs from the configured embedding model. For example, when you have changed the embedding model setting and have not yet run `mdya embedding rebuild`. A running MCP server compares with the settings as they were when the server started.
- Behavior: mdya does not treat it as an error and goes on. Vector search returns no hits. Hybrid search ranks the results with the full-text search side only. mdya writes a warning to stderr, and with `--json` and in MCP, it also puts the warning in the result's `warnings`.

```
WARN mdya::query::search: The index's embeddings were made with cl-nagoya/ruri-v3-30m (256 dimensions), but the query's embedding was made with sentence-transformers/all-MiniLM-L6-v2 (384 dimensions). Vector search finds nothing until you run mdya embedding rebuild, or restart the MCP server if the model setting changed after it started.
```

- How to fix: Run `mdya embedding rebuild` to rebuild the embeddings with the configured embedding model. For a running MCP server, if you changed the embedding model setting after the server started, restart the server.

## The embeddings have not been rebuilt

- When it happens: In update and `mdya put`, when the embedding model that made the index's embeddings differs from the configured embedding model. This does not apply when the index has no chunks yet. In some cases, this also applies after a rebuild of the embeddings was stopped while switching over to the new embeddings. The error message is then `a rebuild of the embeddings was stopped while switching over to the new embedding model`. Exit code 1.
- What remains: mdya ends without writing any documents, so nothing changes. This is so that embeddings from different embedding models are not mixed.

```
Error: the configured embedding model sentence-transformers/all-MiniLM-L6-v2 (384 dimensions) differs from the one the index's embeddings were made with, cl-nagoya/ruri-v3-30m (256 dimensions)

How to fix:
    Rebuild the embeddings with mdya embedding rebuild, then run the operation again.
```

- How to fix: Run `mdya embedding rebuild`, then ingest again.

## Some chunks are longer than the embedding model's context length

An embedding model has a fixed length of text it can take at once (the context length). When a chunk is longer than that, the chunk's embedding represents only the part from the start of the chunk that fits in the context length.

- When it happens: When there are such chunks after an update, `mdya put`, or `mdya embedding rebuild`.
- Behavior: mdya does not treat it as an error and goes on. The exit code does not change. For each collection, mdya writes the collection name and the number of such chunks as a warning (`some chunks are longer than the embedding model's context length, …`). You can also see that number in `truncated` of `mdya status`.
- What remains: Such chunks also go into the index. Full-text search finds text in any part of the chunk. Vector search may not find the chunk by the meaning of the part that did not fit in the context length.
- How to fix: Change to an embedding model with a longer context length (`mdya config set embedding.model`), then rebuild the embeddings with `mdya embedding rebuild`.

## The query is longer than the embedding model's context length

- When it happens: In vector search and hybrid search, when the query is longer than the context length of the configured embedding model. Exit code 1. The MCP `code` is `invalid_input`.
- Behavior: mdya ends without searching. It does not search with only the start of the query, and in hybrid search, it does not search with only the full-text search side either.
- How to fix: Shorten the query, or search with full-text search (`--mode fts`). You can also change to an embedding model with a longer context length. For the MCP server, restart the server after changing the setting.

## The index or settings cannot be read

- When it happens: When the index or settings are broken and cannot be read. For example, the settings file was edited directly and its format was broken, or a disk fault broke part of the index. This also applies when maintenance ([Commands: Maintaining the index](commands.md#maintaining-the-index-mdya-optimize)) removed the state that a search in another process was reading (the index is not broken). Exit code 1. The MCP `code` is `internal`.
- Behavior: mdya ends that operation with an error. mdya does not repair anything by itself, and does not run with default values instead. If a write operation notices this after it has started writing, what it finished writing up to then remains. However, in update, `mdya put`, `mdya embedding rebuild`, and operations that change the embedding model setting, if mdya cannot read the state of the index for the warnings or messages after it has finished the operation, it leaves the operation finished and reports that it could not read it with a warning (with a message for the operations that change the setting). In this case too, the exit code is 1.
- How to fix:
  - If you searched during maintenance: Search again.
  - If you edited the settings file directly: Put it back into a form that can be read, for example by restoring it to how it was before the edit.
  - If that does not fix it, or if the index cannot be read: Delete the settings and index directory (`~/.mdya` by default), then create the collections again and ingest again. Give the documents of raw collections again with `mdya put`. The settings also go back to their default values, so set again any settings you had changed.

## The index or settings cannot be written

- When it happens: In a write operation, when the index or settings cannot be written. For example, the disk is full, or there is no permission to write. The cause (`Caused by:`) shows the reason. Exit code 1. The MCP tools only read, so this does not happen in MCP.
- Behavior: mdya stops there. What it finished writing up to then remains. When the maintenance at the end of a write operation cannot write, mdya leaves the operation finished and writes a warning that the maintenance failed. A failed maintenance does not change the exit code.
- How to fix: Remove the reason (free up disk space, fix the write permissions, and so on), then do the same operation again.

## Another write operation is in progress

- When it happens: When you start a write operation while another write operation (such as `mdya update --all`) is in progress on the same settings and index directory. Exit code 1.
- Behavior: The operation started later ends without waiting and without doing anything. The operation in progress goes on as it is.
- How to fix: After the operation in progress has finished, do the operation again. A write operation that stopped partway does not block the next write operation.

The write operations are: adding, removing, and changing collections, update, `mdya put`, `mdya remove`, changing settings, rebuilding the embeddings, and maintaining the index. Read operations, such as search, return results without waiting, even during a write operation. At that time, documents that the write operation has finished writing appear with their new content, and documents it has not yet written appear with their previous content.

## Interruption

- When it happens: When, partway through an operation, it is stopped with Ctrl+C or the like, or the computer loses power. The exit code follows the rules of the OS.
- What remains: Documents finished writing before the stop remain in their state after writing. A document that was being written is in either its previous state or its state after writing. When a rebuild of the embeddings is stopped, the partly made embeddings are not used for search.
- How to fix: Do the same operation again. An update without `--force` skips documents that have not changed, so it continues from where it stopped. For `mdya embedding rebuild`, rebuilding again finishes the rebuild.

## Memory went over the limit

- When it happens: When mdya's memory usage goes over the memory limit (`resources.memory_limit_mb`). Exit code 137. The MCP server also stops.
- What remains: The same as [Interruption](#interruption).
- How to fix: Raise the memory limit or lower the parallelism, then do the same operation again ([Settings: Memory and parallelism](configuration.md#memory-and-parallelism)).

```
Error: mdya's memory usage went over the limit of 50 MB

How to fix:
    Raise the memory limit (mdya config set resources.memory_limit_mb) or lower the parallelism (mdya config set resources.embedding_parallelism), then run the operation again.
```

## The address to listen on cannot be used

- When it happens: In `mdya mcp --http`, when the address to listen on cannot be used. For example, another process is using that address, or you gave an address that the computer does not have. Exit code 1. When the address is not in a valid form, it is an argument error (exit code 2).
- How to fix: Give a different address or port number with `--address`, or stop the process that is using the address.

```
Error: cannot listen on 127.0.0.1:18765

Caused by:
    0: Address already in use (os error 48)

How to fix:
    Give a different address or port with --address, or, if another process is using the address, stop it.
```

## The connection with the MCP client cannot be started

- When it happens: In `mdya mcp` (stdio), when the connection with the MCP client cannot be started. For example, the first thing that arrived was not a request, or the connection was cut before it started. Exit code 1.
- How to fix: Check the MCP client's settings, and restart the MCP server from the MCP client.

## The settings and index directory cannot be determined

- When it happens: When the settings and index directory cannot be determined. For example, with the default value or a value that starts with `~`, when the home directory is unknown, or with a relative path value, when the working directory cannot be read. Exit code 1.
- How to fix: Give an absolute path with `--data-dir`. When the working directory cannot be read, you can also move to a working directory that can be read and try again.

## A collection has a type this mdya does not know

- When it happens: When this mdya handles a collection of a collection type made with a newer version of mdya. This happens when changing the location of the collection, and in update, `mdya put`, and `mdya remove`. With `mdya update --all`, it happens if there is even one such collection, and no other collection is updated either. Exit code 1.
- How to fix: Use a version of mdya that can handle that collection type, or remove that collection with `mdya collection remove`.

## Unexpected failure

- When it happens: When a bug in mdya makes it unable to continue. The CLI exit code is 101. In the MCP server, when it happens inside one call, mdya returns an error to that call only (`code` is `internal`), and the server keeps running.
- How to fix: Report it as a bug in an [issue](https://github.com/yoshihirosuzuki/mdya/issues). Include the command you ran, the output of `mdya --version`, and what mdya wrote to stderr.

## Argument errors

- When it happens: When a command name or an option is not valid, when a required argument is missing, or when a global option (such as `--data-dir`) is written after the command name. Exit code 2.
- How to fix: Check how to write the command with `mdya --help` or with that command's `--help`.
