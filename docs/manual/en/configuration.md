# Settings

This page describes mdya's settings and what mdya puts on your computer.

## Changing settings

Change a setting with `mdya config set`, and put it back to its default value with `mdya config reset`. You can see the current values with `mdya config list` ([Commands: Changing settings](commands.md#changing-settings-mdya-config-set-mdya-config-reset)). Do not edit the settings file directly. mdya does not guarantee how it behaves if you edit the file directly.

| Setting name | Meaning | Default value | Accepted values |
|---|---|---|---|
| `embedding.model` | [Embedding model](#embedding-model) | `cl-nagoya/ruri-v3-30m` | A model from the list below, or `ollama:<model name>` |
| `embedding.ollama_endpoint` | The Ollama endpoint ([Using Ollama](#using-ollama)) | `http://127.0.0.1:11434` | A URL that starts with `http://` or `https://` |
| `resources.memory_limit_mb` | [Memory limit](#memory-and-parallelism) (MB) | `8192` | An integer, 0 or greater. With 0, memory is not monitored |
| `resources.embedding_parallelism` | [Parallelism](#memory-and-parallelism) | `8` | An integer, 0 or greater. With 0, processing is done one at a time, in order. A value greater than 1024 is treated as 1024 |
| `get.cli_limit_bytes` | Output size limit when getting a document from the CLI (in bytes; [Output size limit for getting documents](#output-size-limit-for-getting-documents)) | `1048576` | An integer, 0 or greater. With 0, no limit is applied |
| `get.mcp_limit_bytes` | Output size limit when getting a document over MCP (in bytes) | `1048576` | An integer, 0 or greater. With 0, no limit is applied |
| `mcp.show_version` | Whether to show the mdya version over MCP ([The MCP server](mcp.md#hiding-the-version)) | `true` | `true` or `false` |

Write integers in decimal, up to 9007199254740991. In this manual, MB means 1,048,576 bytes (1 MiB).

A running MCP server keeps using the settings from when it was started. Changed settings take effect when you restart the server.

## Directories

mdya puts things in the following two directories on your computer. mdya creates each of them the first time it is needed.

| Directory | What it holds | Default location | How to change it |
|---|---|---|---|
| Settings and index directory | The settings, the index, the attributes of collections, and other things mdya needs to run | `~/.mdya` | `--data-dir <directory>` |
| Embedding model cache directory | Downloaded embedding models | `~/.mdya-models` | `--model-cache-dir <directory>` |

- Put `--data-dir` and `--model-cache-dir` right after `mdya`, before the command name. They cannot be set with environment variables.
- A leading `~` is replaced with your home directory. A relative path is resolved from the working directory in which you ran the command.
- If you use a different settings and index directory, you get separate settings and a separate index. Several settings and index directories can share one embedding model cache directory.
- mdya does not guarantee how it behaves if you delete anything inside these directories directly.

## Embedding model

Vector search and hybrid search use an embedding model to represent the meaning of text as embeddings, and search with them. Both the embeddings of the query and the embeddings of the chunks being ingested are made with the configured embedding model (`embedding.model`). Full-text search does not use the embedding model.

You can choose from the following embedding models.

| Value of `embedding.model` | Description |
|---|---|
| `cl-nagoya/ruri-v3-30m` (default) | A Japanese-language model |
| `google/embeddinggemma-300m` | A model that you need permission to use ([Gated models](#gated-models)) |
| `sentence-transformers/all-MiniLM-L6-v2` | An English-language model |
| `ollama:<model name>` | Leaves making the embeddings to Ollama ([Using Ollama](#using-ollama)) |

mdya runs the models other than Ollama's on your computer. The first time a model is used, mdya downloads it from Hugging Face and puts it in the embedding model cache directory (`cl-nagoya/ruri-v3-30m` is about 150 MB). From the second time on, mdya reads it from the cache. So a network connection is needed only the first time a model is used.

### Document language

The default settings are meant for Japanese documents.

- The default embedding model, `cl-nagoya/ruri-v3-30m`, is a Japanese-language model.
- Full-text search splits text into words with a Japanese dictionary (IPADIC). You can also search for English words ([Commands: Searching](commands.md#searching-mdya-search)). It is not adapted to how words are split in other languages.

For documents in languages other than Japanese, you can choose embedding models that may suit them: `google/embeddinggemma-300m`, `sentence-transformers/all-MiniLM-L6-v2` (English), and Ollama models. However, search accuracy has not been verified for documents in languages other than Japanese.

### Changing the embedding model

1. Change the setting with `mdya config set embedding.model <model>`.
2. Run `mdya embedding rebuild` to rebuild the embeddings in the index with the new embedding model. The rebuild does not read the sources again.

Changing the setting does not change the embeddings in the index. When the index has embeddings made with the previous embedding model, until you rebuild them, vector search returns no hits (hybrid search ranks hits using only the full-text search side), and `mdya update` and `mdya put` fail with an error. This is because comparing embeddings made with different embedding models is meaningless. When the index has no chunks yet (when you changed the embedding model before ingesting anything), you do not need to rebuild.

mdya tells embedding models apart by their name and by the number of dimensions of the embeddings they make. If you replace an Ollama model with a model that has a different number of dimensions but keep the same name, mdya also treats it as a different embedding model. If you replace it with a model that has the same name and the same number of dimensions, mdya cannot tell them apart, so rebuild with `mdya embedding rebuild --force`.

### Gated models

`google/embeddinggemma-300m` is a model on Hugging Face that you need permission to use.

1. On Hugging Face, open the [model page](https://huggingface.co/google/embeddinggemma-300m), agree to the terms of use, and get permission.
2. Run mdya with your Hugging Face access token in the `HF_TOKEN` environment variable. The token is used when downloading the model.

### Using Ollama

When you set `embedding.model` to `ollama:<model name>`, mdya leaves making the embeddings to Ollama. `ollama:` without a model name after it is not accepted.

- Install Ollama separately and have it running. Also have the model you use ready in Ollama.
- You can change the Ollama endpoint with `embedding.ollama_endpoint`. If you set it to an endpoint outside your computer, the text to be embedded is sent outside your computer.

### Proxies

Downloading the embedding models that run on your computer, and communicating with an Ollama outside your computer, follow the proxy environment variables. mdya has no setting or argument for specifying a proxy.

- The environment variables read are `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, `NO_PROXY`, and the lowercase name of each. Connections to destinations listed in `NO_PROXY` do not go through a proxy.
- When the host of the Ollama endpoint is written as `localhost` or as a loopback address (`127.0.0.0/8`, `::1`, and `127.0.0.0/8` addresses written in IPv6 form, such as `::ffff:127.0.0.1`), connections do not go through a proxy, regardless of the proxy environment variables. This is so that text sent to an Ollama on the same computer is not sent to a proxy.
- Only http and https proxies can be used. SOCKS proxies cannot be used; if one is set, communication that goes through the proxy fails with an error. The operating system's proxy settings (System Settings on macOS, the registry on Windows) are not read.

## Memory and parallelism

mdya monitors its own memory usage (the amount of resident memory), and stops immediately when it goes over the memory limit (`resources.memory_limit_mb`, 8192 MB by default). This is to keep your computer from freezing. Because processes other than mdya also use the computer's memory, mdya does not guarantee that the computer as a whole will not freeze.

The parallelism (`resources.embedding_parallelism`, 8 by default) is the number of documents processed at the same time when making embeddings. The higher the parallelism, the more memory mdya uses.

### A measured example

This is one example measured with the default values (parallelism 8, memory limit 8192 MB). With a different set of documents or a different computer, the memory used will differ.

- Documents: 188 Markdown files, about 2.9 MiB in total. Split into two directory collections.
- Embedding model: `cl-nagoya/ruri-v3-30m`.
- Computer: Apple M4, 16 GB of memory, macOS.

| Operation | Peak memory usage in each run (range over 3 runs) |
|---|---|
| Ingestion (`mdya update --all`) | 1172.5–1210.9 MB |
| Rebuilding the embeddings (`mdya embedding rebuild --force`) | 1092.2–1165.3 MB |

### When memory goes over the limit

mdya stops with exit code 137 and writes the following to stderr (an example with the memory limit set to 50 MB).

```
Error: mdya's memory usage went over the limit of 50 MB

How to fix:
    Raise the memory limit (mdya config set resources.memory_limit_mb) or lower the parallelism (mdya config set resources.embedding_parallelism), then run the operation again.
```

Raise the memory limit (`mdya config set resources.memory_limit_mb <MB>`) or lower the parallelism (`mdya config set resources.embedding_parallelism <number>`), then run the same operation again. Documents that were completely written before mdya stopped are kept. An update without `--force` skips documents that have not changed, so it continues from where it stopped.

In an archive collection, each entry is limited to 256 MiB after decompression, and the documents in the archive are read one at a time ([Collections and collection types: Size and memory](collections.md#size-and-memory)).

## Output size limit for getting documents

This is the limit on the number of UTF-8 bytes in the text that `mdya get` and the MCP tool `get_document` return (the whole body of a document, or the text of a range of chunks). It is there so that getting a large document by accident does not fill your terminal or your conversation with an AI assistant.

- `get.cli_limit_bytes`: the limit for the CLI. The default is 1048576 (1 MiB). With `mdya get --no-size-limit`, you can remove the limit for that one run only.
- `get.mcp_limit_bytes`: the limit for MCP. The default is 1048576 (1 MiB). There is no way to remove the limit for a single call.

When the text is over the limit, nothing is returned and an error occurs ([The output is too large](errors.md#the-output-is-too-large)). Specify ranges of chunks and get the text in parts.

## Logs and color

mdya writes logs only to stderr. Messages (such as that an operation succeeded, progress, and the summary on completion) are not logs, so they are written even if you change the log level.

| What you set | How to set it | Default |
|---|---|---|
| Log level | Give one of `error`, `warn`, `info`, `debug`, or `trace` with `--log-level`. You can also set it for each target that writes logs (`mdya=debug`). Without `--log-level`, the `RUST_LOG` environment variable is used | `warn` (warnings and above). For the part of a library mdya uses that builds the index for fast vector search, `error` (errors only) |
| Log format | Give one of `compact` (one entry per line), `pretty` (formatted for reading), or `json` with `--log-format` | `compact` |
| Color | With `--no-color`, or when the `NO_COLOR` environment variable is present, output is not colored | Each of stdout and stderr is colored only when it is a terminal |

When you set the log level with `--log-level` or `RUST_LOG`, your setting is used instead of the default. For example, with `--log-level warn`, the warnings from the part that builds the index are written too.

If part of `RUST_LOG` is not valid, only that part is ignored, and a warning saying so is written.

To keep logs in a file (for example, when you keep the MCP server running), redirect stderr to a file in your shell.

```
mdya mcp --http 2>> ~/mdya-mcp.log
```
