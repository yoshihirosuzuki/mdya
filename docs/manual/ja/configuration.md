# 設定

mdya の設定は `~/.mdya/config.yml` の 1 ファイルにまとまっています。`mdya init` がひな形を生成し、`mdya collection add` がコレクションのセクションを書き換えます。多くの場合手で編集する必要はありませんが、`embedding` / `runtime` / `get` の値を変えたいときは直接編集できます。

## ディレクトリレイアウト

`mdya init` を実行すると以下が生成されます。

```
~/.mdya/
├── config.yml      # ユーザが編集する設定ファイル
├── index/          # 索引データ
└── lance-models/   # 全文検索の形態素解析辞書設定
```

各エントリの役割:

- `config.yml` — 設定の単一情報源です。
- `index/` — `mdya update-all` が構築する索引の置き場です。
- `lance-models/` — 全文検索の日本語形態素解析に使う辞書設定の置き場です。`mdya init` が用意します。

埋め込みモデルのキャッシュは設定ディレクトリの外、default で `$HOME/.mdya-models/` に置かれます。初回の `mdya update-all` やベクトル / hybrid 検索で約 140 MB のモデルが自動ダウンロードされ、以降はこのキャッシュを使います。`mdya init` はこのディレクトリを作りません (初回のモデル読み込み時に自動で作られます)。

複数の設定ディレクトリで 1 つのモデルキャッシュを共有したり、設定ディレクトリを読み取り専用でマウントしたりできます。

## 設定ディレクトリの切り替え

`~/.mdya/` 以外の場所を使いたい場合は `--config-dir <path>` フラグ (全サブコマンドに付与可能) で上書きします。未指定なら `$HOME/.mdya/` を使います。

埋め込みモデルキャッシュの場所も `--model-cache-dir <path>` フラグで上書きできます。未指定なら `$HOME/.mdya-models/` を使います。

例:

```sh
mdya --config-dir ./scratch-mdya init
mdya --config-dir ./scratch-mdya --model-cache-dir /shared/mdya-models search fts "release"
```

`~/...` 記法は config 内の `path` でも引数でも展開されます。

## config.yml の全項目

最小例:

```yaml
collections:
  notes:
    path: ~/notes
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime:
  memory_limit_mb: 8192
  embed_parallelism: 8
get:
  cli_max_bytes: 1048576
  mcp_max_bytes: 1048576
```

セクションは 4 つあります。

### collections

検索対象として登録したディレクトリの一覧です。

```yaml
collections:
  notes:
    path: ~/notes
    description: 個人メモ
  work:
    path: /Users/me/work/docs
```

- キー (`notes` / `work`) がコレクション名になります。`mdya search ... -c notes` のようにフィルタで使います。
- `path` (必須) は索引対象のディレクトリです。`~/...` は展開されます。
- `description` (任意) は人間向けの説明文です。`mdya collection list` で表示されます。

`mdya collection add <path>` で追加するのが普通で、その場合キー名は `<path>` の basename になります (`--name` で上書き可)。

`mdya update-all` の走査は**コレクションルート配下のシンボリックリンクを辿りません**。ルート自身がシンボリックリンクの場合 (例: `~/notes -> ~/Dropbox/notes`) は辿ります。別ディスクに散らばった領域を索引化したいときは、別コレクションとして追加してください。

### embedding

ベクトル検索 / hybrid 検索で使う埋め込みモデルを 1 つだけ指定します。

```yaml
embedding:
  model: cl-nagoya/ruri-v3-30m
```

- `model` — モデル ID。default は `cl-nagoya/ruri-v3-30m` (256 次元、日本語に強い、約 140 MB、Apache 2.0)。`ollama:<name>` 形式で Ollama を指すこともできます (詳細は [`commands.md` の `mdya vector use`](commands.md#mdya-vector-use-model))。
- モデルを変えたいときは `config.yml` を手で書き換えるのではなく `mdya vector use <model>` を使ってください。索引と config が整合した状態で切り替わります。

`config.yml` の `embedding.model` と索引内のモデルが食い違うと、`mdya update-all` は安全のため停止し、`mdya search vector` / `hybrid` は警告を出して検索を続けます。

### runtime

実行時の挙動の調整つまみです。

```yaml
runtime:
  memory_limit_mb: 8192
  embed_parallelism: 8
```

#### memory_limit_mb

mdya プロセス自身の使用メモリ (RSS) の上限を MB で指定します。default は `8192` (= 8 GB)。`0` で無効化されます。

上限を超えると mdya はエラーを 1 行出して即終了します (終了コード 137)。これは大きなコレクションを索引化中に PC ごとハングするのを避けるための安全装置です。終了したら下記の `embed_parallelism` を下げるか、`memory_limit_mb` を上げてください。

#### embed_parallelism

`mdya update-all` で並列に埋め込みを計算するファイル数の上限です。default は `8`。`0` で逐次処理になります。

並列度を上げると速くなる一方、メモリ使用量も比例して増えます。ワーストケースで 1 ファイルあたり約 1.5 GB を消費するため、`memory_limit_mb` との関係を意識して調整してください:

- 並列度 `8` × 1.5 GB ≈ 12 GB (上限 8 GB を超える可能性あり)
- 並列度 `4` × 1.5 GB ≈ 6 GB
- `0` (逐次) ≈ 1.5 GB

メモリ上限に引っかかって 137 で落ちる場合は、まず並列度を下げてみてください。

### get

`mdya get` と MCP `get_document` ツールが返す document 全文のサイズ上限です。単位は UTF-8 バイト数です。

```yaml
get:
  cli_max_bytes: 1048576
  mcp_max_bytes: 1048576
```

#### cli_max_bytes

`mdya get` がエラーで停止せずに出力する document 全文の最大サイズです。default は `1048576` (= 1 MiB)。`0` で無効化されます。

`mdya get` に `-f` / `--no-size-limit` を渡すと、その 1 回だけ上限を無視して出力します (大きな document を意図的にリダイレクト / パイプするときに便利)。上限が効くのは全文取得のみで、`mdya get --chunk <N>` は対象外です。

#### mcp_max_bytes

MCP `get_document` ツールが `payload_too_large` エラーを返さずに返す document 全文の最大サイズです。default は `1048576` (= 1 MiB)。`0` で無効化されます。MCP 側にはリクエスト単位の上書きはありません。CLI と同様に `chunk` 取得は対象外です。
