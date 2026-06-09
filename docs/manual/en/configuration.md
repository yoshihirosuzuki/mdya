# Configuration

mdya's configuration lives in a single file at `~/.mdya/config.yml`. `mdya init` generates a template, and `mdya collection add` rewrites the collections section. You usually do not need to edit this file by hand, but you can edit the `embedding` and `runtime` values directly when you need to.

## Directory layout

Running `mdya init` produces the following structure.

```
~/.mdya/
├── config.yml      # The configuration file you edit
├── index/          # Index data
└── lance-models/   # Full-text search tokenizer configuration
```

What each entry does:

- `config.yml` — the single source of truth for configuration.
- `index/` — where `mdya update-all` builds the index.
- `lance-models/` — where the dictionary configuration used by the Japanese morphological analyzer lives. Created by `mdya init`.

The embedding model cache sits outside the configuration directory, defaulting to `$HOME/.mdya-models/`. The first `mdya update-all` or vector / hybrid search downloads the model (about 140 MB) into this cache and reuses it afterwards. `mdya init` does not create this directory (it is created automatically when the model is first loaded).

This split lets you share one model cache across multiple configuration directories, or mount the configuration directory read-only.

## Relocating the configuration directory

To use a directory other than `~/.mdya/`, pass `--config-dir <path>` (available on every subcommand) to override it. Without the flag, mdya uses `$HOME/.mdya/`.

The embedding model cache location can similarly be overridden with `--model-cache-dir <path>`. Without the flag, mdya uses `$HOME/.mdya-models/`.

Example:

```sh
mdya --config-dir ./scratch-mdya init
mdya --config-dir ./scratch-mdya --model-cache-dir /shared/mdya-models search fts "release"
```

The `~/...` shorthand is expanded both inside `path` values in the configuration and in command-line arguments.

## Every option in config.yml

Minimal example:

```yaml
collections:
  notes:
    path: ~/notes
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime:
  memory_limit_mb: 8192
  embed_parallelism: 8
```

There are three sections.

### collections

The list of directories registered as search targets.

```yaml
collections:
  notes:
    path: ~/notes
    description: Personal notes
  work:
    path: /Users/me/work/docs
```

- The key (`notes` / `work`) is the collection name. Use it in filters like `mdya search ... -c notes`.
- `path` (required) is the directory to index. `~/...` is expanded.
- `description` (optional) is a human-readable description. Shown by `mdya collection list`.

The usual way to add a collection is `mdya collection add <path>`, in which case the key is set to the basename of `<path>` (overridable with `--name`).

`mdya update-all` walks the directory and **does not follow symbolic links under the collection root**. When the root itself is a symbolic link (e.g. `~/notes -> ~/Dropbox/notes`), the root link is followed. If you want to index areas scattered across separate disks, register each one as its own collection.

### embedding

Specifies exactly one embedding model used by vector / hybrid search.

```yaml
embedding:
  model: cl-nagoya/ruri-v3-30m
```

- `model` — model ID. The default is `cl-nagoya/ruri-v3-30m` (256 dimensions, strong for Japanese, about 140 MB, Apache 2.0). The `ollama:<name>` form points at an Ollama model (see [`mdya vector use` in commands.md](commands.md#mdya-vector-use-model) for details).
- When you want to change the model, do not edit `config.yml` by hand — use `mdya vector use <model>`. That keeps the index and the configuration in sync across the switch.

When `embedding.model` in `config.yml` disagrees with the model recorded in the index, `mdya update-all` stops for safety and `mdya search vector` / `hybrid` print a warning and continue.

### runtime

Knobs that adjust runtime behavior.

```yaml
runtime:
  memory_limit_mb: 8192
  embed_parallelism: 8
```

#### memory_limit_mb

The upper bound on mdya's own resident memory (RSS), in MB. Default `8192` (= 8 GB). Set to `0` to disable.

When the limit is exceeded, mdya prints a one-line error and exits immediately (exit code `137`). This is a safety net that prevents an entire workstation from hanging while a large collection is being indexed. After such an exit, lower `embed_parallelism` below or raise `memory_limit_mb`.

#### embed_parallelism

The maximum number of files embedded in parallel during `mdya update-all`. Default `8`. Set to `0` for sequential processing.

Higher parallelism is faster but proportionally increases memory usage. Worst case is roughly 1.5 GB per in-flight file, so size this against `memory_limit_mb`:

- Parallelism `8` × 1.5 GB ≈ 12 GB (may exceed the 8 GB default limit)
- Parallelism `4` × 1.5 GB ≈ 6 GB
- `0` (sequential) ≈ 1.5 GB

If you hit the memory limit and the process exits with `137`, try lowering parallelism first.
