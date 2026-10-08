# MCP サーバ連携

mdya は MCP (Model Context Protocol) サーバとして起動でき、Claude Code / Claude Desktop などの MCP クライアントから検索ツールを呼び出せます。

## 起動方法

```sh
mdya mcp                                  # stdio (default)
mdya mcp --http                           # HTTP (foreground daemon)
mdya mcp --http --addr 127.0.0.1:9000     # bind address を指定
```

| モード | 用途 | 備考 |
|---|---|---|
| stdio (default) | エディタ統合 (Claude Code 等) | stdout は MCP プロトコル専用、ログは stderr へ |
| `--http` | バックグラウンド常駐 | foreground プロセス。シェル側で `&` / `nohup` / systemd 等で背景化 |

`--http` の bind address は default が `127.0.0.1:8000` (loopback) です。リモート公開は想定外で、loopback 以外の Host ヘッダはサーバが拒否します。

`--http` daemon は設定ディレクトリ単位で 1 つだけ起動できます (`~/.mdya/mcp.pid` を排他ロックで保持)。2 つ目を起動しようとすると拒否されます。

stdio モードはエディタ統合用の設計です。クライアント側でプロセス管理する前提で動きます。

## Claude Code への登録

```sh
claude mcp add mdya -- mdya mcp
```

これで `mdya` という名前で MCP サーバが登録され、ツールが利用可能になります。

## 提供ツール

mdya MCP サーバは次のツールを提供します。

### 検索ツール

| ツール名 | 内容 |
|---|---|
| `search` | Markdown コレクションの検索。`mode` でバックエンド (BM25 / ベクトル / ハイブリッド) を選択 |

入力スキーマ:

```json
{
  "query": "リリース手順",
  "mode": "hybrid",
  "k": 20,
  "collections": ["notes"],
  "level": "doc"
}
```

- `query` (必須) — 検索クエリ文字列。空文字列 / 空白のみはエラー。
- `mode` (任意、default `"hybrid"`) — 検索バックエンド。`"fts"` (BM25 の語句一致)、`"vector"` (cosine の意味検索)、`"hybrid"` (両者を RRF で統合)。
- `k` (任意、default `20`) — 上位 N 件。`0` はエラー。
- `collections` (任意) — コレクション名で絞り込み。省略 / 空配列で全コレクション対象。未知のコレクション名はエラー。
- `level` (任意、default `"doc"`) — hit 粒度。`"doc"` は文書単位の集約 hit、`"chunk"` はチャンク単位の hit。

出力は CLI の `--format json` と同じエンベロープです。

```json
{
  "query": "リリース手順",
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

`level: "doc"` (default) では文書単位の集約 hit が返り、 hit の `matched_chunks` に文書内で hit したチャンク数が入ります。 入力で `level: "chunk"` を渡すとチャンク単位の hit が返り、 hit に `chunk_sequence` (0-indexed のチャンク番号) が入ります。 どちらの粒度でも、 hit の `chunk_count` にその文書のチャンク総数が入ります。

スコアの数値域は mode ごとに異なります ([commands.md の 3 モードの違い](commands.md#3-モードの違い) を参照)。

### 文書取得ツール

| ツール名 | 内容 |
|---|---|
| `get_document` | 1 つの文書の原文を取得 |

入力:

```json
{
  "collection": "notes",
  "path": "release.md"
}
```

出力:

```json
{
  "collection": "notes",
  "path": "release.md",
  "content": "..."
}
```

- `collection` (必須) — コレクション名 (`config.yml` で宣言済みのもの)。
- `path` (必須) — コレクションルートからの相対パス。
- `chunk` (任意) — 0-indexed の `chunk_sequence`。指定すると document 全文ではなく、そのチャンクにあたる部分の原文を返します。省略時は忠実な全文を返します。
- `chunk_end` (任意) — `chunk` と組み合わせ、チャンク `chunk` から `chunk_end` まで (`chunk_end` を含む) を 1 続きの原文として返します。`chunk` 以上の値を指定します。最後のチャンクを超える値は文書の末尾までとして扱います。

検索ヒットの `collection` と `path` をそのまま渡せば原文が取れます。hit の `chunk_sequence` を `chunk` に渡せばそのチャンクだけを取得できます。前後も含めて読むときは、`chunk` に hit より小さい番号、`chunk_end` に大きい番号を渡します。チャンクにあたる部分の原文には、見出し記号・リンク先・HTML などの書式も元のまま含まれます。チャンクの範囲は文書を隙間なく区切っているので、1 回の範囲指定の中では隣り合うチャンクの間に重複は出ません。ただし、1 つの長いブロック (長い段落・コードブロック、または PDF) を切ったチャンクは前後と少し重なるので、別々に取得した結果の境目がそうしたブロックの途中に来ると、つないだときに継ぎ目で少し重複します。

全文が `get.mcp_max_bytes` を超える文書も、多くの場合は `chunk` / `chunk_end` で上限に収まる範囲ずつ読み進められます。本文に文字を持たない大きな内容 (巨大な HTML ブロックなど) を含み、1 チャンクの範囲だけで上限を超える部分は読めません。チャンクの総数は検索 hit の `chunk_count` で分かります。

### 索引情報ツール

| ツール名 | 内容 |
|---|---|
| `list_collections` | 登録済みコレクションの一覧 |

入力は不要です。出力は CLI の `mdya collection list --format json` と同じ shape です ([commands.md](commands.md) を参照)。

## エラー時の動作

ツール呼び出しが失敗すると、構造化されたエラーが返ります。

```json
{
  "code": "unknown_collection",
  "message": "unknown collection: 'foo'",
  "details": { "collection": "foo" }
}
```

| `code` | 発生条件 |
|---|---|
| `empty_query` | `query` が空文字列 / 空白のみ |
| `invalid_limit` | `k` が `0` |
| `unknown_collection` | 未知のコレクション名を指定 |
| `not_found` | `get_document` で該当文書が無い、または指定した `chunk` が範囲外 |
| `invalid_chunk_range` | `get_document` で `chunk` なしに `chunk_end` を指定した、または `chunk_end` が `chunk` より小さい |
| `payload_too_large` | `get_document` の応答 (全文、または `chunk` / `chunk_end` の範囲) が `get.mcp_max_bytes` を超過。`details` に `size_bytes` / `limit_bytes` が入る。`chunk` / `chunk_end` の範囲を狭めて絞り込める |
| `index_outdated` | 索引がチャンクの範囲を記録する前の mdya で作られている。`search` と、`chunk` を指定した `get_document` で返る。`details.embedding_model` のモデル名で `mdya vector use <model>` を実行すると作り直せる。全文の `get_document` はそのまま使える。作り直した後は MCP サーバを再起動する |
| `schema_metadata_missing` | 索引が未初期化 / 壊れている |
| `internal` | I/O / 埋め込み / 設定読み込み等の障害 |

クライアントは `code` を見て挙動を分けられます。`details` はコードに応じて追加情報を含みます。

## ログ出力

`mdya mcp` のログはすべて stderr に出ます。stdio モードでは stdout が MCP プロトコルチャネルとして占有されているため、ログを stdout に混ぜるとプロトコルが壊れます。

`--http` daemon を背景常駐させてログをファイルに残したい場合はシェルでリダイレクトしてください。

```sh
nohup mdya mcp --http > ~/.mdya/mcp.log 2>&1 &
```
