# コマンドリファレンス

mdya のサブコマンド一覧と詳細です。すべてのサブコマンドで以下のグローバルフラグが使えます。

## グローバルフラグ

| フラグ | 役割 | default |
|---|---|---|
| `--config-dir <PATH>` | 設定ディレクトリの上書き | `$HOME/.mdya/` |
| `--model-cache-dir <PATH>` | 埋め込みモデルキャッシュディレクトリの上書き | `$HOME/.mdya-models/` |
| `--log-level <LEVEL>` | ログの詳細度 (`trace` / `debug` / `info` / `warn` / `error`)。`RUST_LOG` 環境変数でも可 | `warn` |
| `--log-format <FORMAT>` | ログのフォーマット (`compact` / `pretty` / `json`) | `compact` |
| `--no-color` | 端末色出力の抑制。`NO_COLOR` 環境変数でも可 | TTY 自動判定 |
| `-h, --help` | ヘルプ表示 | — |
| `-V, --version` | バージョン表示 | — |

ログはすべて stderr に出ます。stdout は検索結果や MCP プロトコルメッセージなどデータ専用です。

## 終了コード

| コード | 意味 |
|---|---|
| `0` | 成功 |
| `1` | アプリケーションエラー (IO / 設定不整合 / 検索失敗など) |
| `2` | 引数のパースエラー |
| `101` | 内部エラー (パニック) |
| `137` | メモリ上限超過による即時終了 ([configuration.md の `memory_limit_mb`](configuration.md#memory_limit_mb) 参照) |

---

## `mdya init`

`~/.mdya/` を初期化します。

```sh
mdya init
```

- `config.yml` を default 値で生成 (既にあれば触りません)。
- `index/` / `lance-models/` を作成 (既にあれば触りません)。
- 埋め込みモデルのキャッシュは `~/.mdya-models/` (`--model-cache-dir` で変更可) に置かれ、初回の `mdya update-all` やベクトル / hybrid 検索で自動ダウンロードされます。`mdya init` はこの dir を作りません。

何度実行しても安全です。やり直したい場合は `~/.mdya/` を手動で削除してから再実行してください。

---

## `mdya collection add <path>`

ディレクトリをコレクションとして登録します。

```sh
mdya collection add <path> [--name <NAME>] [--description <TEXT>]
```

| 引数 / オプション | 役割 |
|---|---|
| `<path>` (必須) | 登録するディレクトリ。`~/...` は展開される |
| `--name <NAME>` | コレクション名を上書き (default: `<path>` の basename) |
| `--description <TEXT>` | 人間向けの説明 (`mdya collection list` で表示) |

`config.yml` の `collections` セクションに 1 行追加されるだけで、索引化はここでは行われません。索引化は `mdya update-all` を実行したときに動きます。

例:

```sh
mdya collection add ~/notes
mdya collection add ~/work --name work-docs --description "業務メモ"
```

---

## `mdya collection list`

登録済みコレクションを一覧表示します。

```sh
mdya collection list [--format <FORMAT>]
```

- `--format` は `human` (default) / `json` / `md` / `xml` から選びます。
- 各コレクションについて `name` / `path` / `description` / `document_count` (取り込み済み文書数) を表示します。
- `document_count` は索引内の文書数なので、`mdya update-all` をまだ走らせていないコレクションは `0` になります。

```sh
$ mdya collection list
notes  ~/notes        個人メモ    42 docs
work   ~/work          (no desc)    0 docs
```

`json` で出すとスクリプトから扱いやすいエンベロープになります。

```sh
$ mdya collection list --format json
{
  "collections": [
    { "name": "notes", "path": "~/notes", "description": "個人メモ", "document_count": 42 }
  ]
}
```

---

## `mdya update-all`

登録済みの全コレクションを走査し、新規 / 変更された `.md` / `.markdown` / `.pdf` ファイルを取り込み、削除されたファイルに対応する索引データを掃除します。

```sh
mdya update-all
```

- 索引対象は `.md` / `.markdown` / `.pdf` 拡張子のファイルです。`.pdf` は取り込み時に plain text に変換して Markdown と共通の chunker と embedding に流します。
- コレクションルート配下のシンボリックリンクは辿りません ([configuration.md の `collections`](configuration.md#collections) 参照)。
- 並列度は `runtime.embed_parallelism` で調整します。
- 進捗は stderr にプログレスバーで表示され、最後に stdout へサマリ 1 行を出します。

```
Indexed 43 documents (new: 5, updated: 3, skipped: 34, removed: 0, failed: 1).
```

各カウンタの意味:

- `new` — 新規に取り込んだファイル数
- `updated` — 内容が変わったので再取り込みしたファイル数
- `skipped` — 変更がなくスキップしたファイル数
- `removed` — ファイルが消えたので索引から消した数
- `failed` — 取り込みに失敗したファイル数

`failed > 0` の場合は終了コード `1` を返します (サマリは表示されます)。

---

## `mdya search fts|vector|hybrid <query>`

3 つのモードで検索します。引数とオプションは 3 モードで共通です。

```sh
mdya search fts    <query> [-c <NAMES>...] [-n <N>] [--chunks] [--format <FORMAT>]
mdya search vector <query> [-c <NAMES>...] [-n <N>] [--chunks] [--format <FORMAT>]
mdya search hybrid <query> [-c <NAMES>...] [-n <N>] [--chunks] [--format <FORMAT>]
```

| 引数 / オプション | 役割 | default |
|---|---|---|
| `<query>` (必須) | 検索クエリ文字列。空文字列はエラー | — |
| `-c, --collections <NAMES>` | 対象コレクションの絞り込み。CSV (`-c notes,work`) または繰り返し (`-c notes -c work`) で複数指定可 (OR)。スペース区切り (`-c notes work`) は受け付けない | 全コレクション |
| `-n, --limit <N>` | 上位 N 件 | `20` |
| `--chunks` | hit 粒度をチャンク単位にする。 off (default) は文書単位の集約 hit を返し、 on にすると文書内で hit したチャンクごとに 1 行を返す | off |
| `--format <FORMAT>` | 出力形式 (`human` / `json` / `md` / `xml`) | `human` |

### 3 モードの違い

- `fts` — BM25 による全文検索。語句一致を見ます。日本語形態素解析に対応しています。
- `vector` — cosine 類似度によるベクトル検索。意味的な近さを見ます。初回実行時に埋め込みモデル (約 140 MB) を `~/.mdya-models/` にダウンロードします。
- `hybrid` — `fts` と `vector` の結果を Reciprocal Rank Fusion (RRF) で統合します。

スコアの数値域はモードごとに異なります:

- `fts` — BM25 のスコア (上限なし、値そのものに絶対的な意味はありません)
- `vector` — `[0, 1]` の cosine 類似度
- `hybrid` — RRF の合算値 (上限約 `0.033`)

モードを跨いでスコア値を直接比較しないでください。

### 出力フォーマット

#### `--format human` (default)

```
notes/release.md  score=0.812
  > リリース手順は以下の通り
  > 1. バージョン番号を更新する
---
notes/checklist.md  score=0.751
  > 公開前チェックリスト
---
2 doc hits (showing 20 max)
```

#### `--format json`

機械処理向けエンベロープです。`jq` などで加工できます。

```sh
mdya search fts "release" --format json | jq '.hits[].path'
```

エンベロープの構造:

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
      "matched_chunks": 3
    }
  ]
}
```

- `level` は hit 粒度です。default の `doc` では文書単位の集約 hit を返し、 hit の `matched_chunks` に文書内で hit したチャンク数が入ります。 `--chunks` を渡すと `level: "chunk"` になり、 hit はチャンク単位で `chunk_sequence` (0-indexed のチャンク番号) を含みます。
- `total` は `hits` 配列の件数 (= `limit` で切り詰めた後) と一致し、`level: "doc"` のときは文書数、`level: "chunk"` のときはチャンク数を表します。
- ヒットは `score` の降順に並びます。
- スクリプト用途では必ず終了コードを確認してください。`json` フォーマットでエラーが起きると stdout は空になります (`{"hits":[]}` ではない) ので、stdout の有無だけで成否を判定するとミスリードします。

#### `--format md`

Markdown に整形した人間 / agent 向けビューです。複数行 snippet を blockquote で出すので読みやすくなります。LLM のコンテキストに貼り付ける用途にも向きます。

#### `--format xml`

`json` の構造を 1:1 で写した XML です。

### エラー時の動作

`human` 以外のフォーマットで検索に失敗した場合、stdout には何も出さず、stderr に `human` 形式でエラーが出ます。スクリプトからは終了コードで成否を判定してください。

---

## `mdya get <collection> <path>`

文書の原文を stdout に出力します。検索ヒットから元の文書を取り出すときに使います。

```sh
mdya get <collection> <path> [--chunk <N>] [-f]
```

| 引数 | 役割 |
|---|---|
| `<collection>` (必須) | コレクション名 (`config.yml` で宣言済みのもの) |
| `<path>` (必須) | コレクションルートからの相対パス |

| オプション | 役割 |
|---|---|
| `--chunk <N>` | document 全文ではなくチャンク `N` (0-indexed) の本文だけを出力する。サイズ check は行わない |
| `-f`, `--no-size-limit` | `get.cli_max_bytes` の上限を無視してサイズに関わらず出力する |

例:

```sh
mdya get notes release.md
```

ファイルシステムではなく索引内に保存された原文を返します。`mdya update-all` を実行していない / 取り込まれていない文書は取得できません。

`mdya get` は document が `get.cli_max_bytes` (default 1 MiB、[`configuration.md` の `cli_max_bytes`](configuration.md#cli_max_bytes) 参照) を超えると既定でエラー停止し、巨大なファイルをうっかり取得して端末を埋め尽くすのを防ぎます。意図的に大きな document をリダイレクト / パイプするときは `-f` / `--no-size-limit` を渡すとそのまま出力できます。

---

## `mdya status`

索引の状態を表示します。

```sh
mdya status [--format <FORMAT>]
```

| オプション | 役割 | default |
|---|---|---|
| `--format <FORMAT>` | 出力形式 (`human` / `json` / `md` / `xml`) | `human` |

返される情報:

- `version` — mdya のバージョン
- `embedding_model` — 索引に埋め込んだモデル ID
- `vector_dim` — ベクトル次元数
- `collections` — 登録済みコレクション数
- `chunks` — 索引内のチャンク総数
- `sources` — 索引内の文書総数

`mdya init` 済みであることを前提とします。初期化されていなければエラーになります。

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

埋め込みモデルを切り替えます。**破壊的な操作**です。

```sh
mdya vector use <model> [--yes]
```

| 引数 / オプション | 役割 |
|---|---|
| `<model>` (必須) | 新しいモデル ID。例: `cl-nagoya/ruri-v3-30m` / `ollama:nomic-embed-text` |
| `-y, --yes` | 確認プロンプトをスキップ (非対話 stdin では必須) |

### 何が起きるか

1. `config.yml` の `embedding.model` を新しいモデルに書き換え
2. 索引内のベクトルテーブルを削除
3. 登録済みコレクションを全部スキャンし、新しいモデルで再度埋め込み計算

文書本文の索引 (`mdya get` で取れる原文) はモデル非依存なので残ります。再計算が走るのはベクトル部分だけです。

### 確認プロンプト

対話端末では切り替え内容を表示してから `[y/N]` で確認します。`y` または `yes` を入力すると進みます。それ以外 (空入力含む) は中止します。

```
This will switch the embedding model:
  cl-nagoya/ruri-v3-30m -> ollama:nomic-embed-text (dim 768)
The chunks index will be DROPPED and 3 collection(s) re-embedded from scratch.
Proceed? [y/N]:
```

スクリプトなど非対話 stdin で実行する場合は `--yes` を付けてください。`--yes` なしの非対話実行は安全のため拒否します。

### 完了サマリ

```
Switched embedding model to 'ollama:nomic-embed-text'. Re-embedded 312 document(s) (removed: 0, failed: 0).
```

`failed > 0` の場合は終了コード `1` を返します。残りは `mdya update-all` の通常実行で再開できます。

### Ollama を使う場合

`ollama:<model>` 形式を指定すると、Ollama の埋め込み API を使います。事前に Ollama を起動し、対象モデルを `ollama pull <model>` などで用意しておいてください。

### gated モデルを使う場合

gated な `google/embeddinggemma-300m` に切り替えるには、事前に Hugging Face でのライセンス同意とログインが必要です ([`configuration.md` の `embedding`](configuration.md#embedding) を参照)。

---

## `mdya mcp`

MCP (Model Context Protocol) サーバを起動します。Claude Code 等のクライアントから検索ツールを呼び出せるようになります。

詳細は [`mcp.md`](mcp.md) を参照してください。

```sh
mdya mcp              # stdio で起動 (default)
mdya mcp --http       # HTTP で起動 (foreground daemon)
mdya mcp --http --addr 127.0.0.1:9000  # bind address を指定
```

---

## `mdya version`

mdya のバージョンを 1 行で表示します。

```sh
$ mdya version
mdya v0.0.0
```

スクリプトや MCP クライアントから安定して読み取れる出力面として用意されています。
