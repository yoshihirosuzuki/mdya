# MCP のサーバー

`mdya mcp` は、MCP のサーバーとして動き、AI アシスタント (Claude Code など) から、mdya の文書を検索できるようにします。

## MCP でできることと、できないこと

MCP のツールは、次の 4 つです。どれも読み取りだけで、mdya の中身を変えるものはありません。

| ツール | すること |
|---|---|
| `list_collections` | collection の一覧を見る |
| `search` | 検索する |
| `get_document` | 文書を取得する |
| `get_version` | mdya のバージョンを見る |

collection を作る、取り込む、文書を削除する、設定を変える、といった操作は、MCP ではできません。CLI のコマンドで行ってください ([コマンド](commands.md))。MCP のサーバーを動かしたまま CLI で取り込んだ文書は、サーバーを始め直さなくても、次の呼び出しから検索できます。

## 始め方

| 動かし方 | 始め方 |
|---|---|
| stdio | MCP のクライアントが `mdya mcp` を起動し、標準入力と標準出力でやりとりする |
| HTTP | 利用者が `mdya mcp --http` を動かしておき、MCP のクライアントが HTTP でつなぐ |

stdio では、標準出力を MCP のやりとりだけに使います。ログは stderr に出ます。

HTTP では、パス `/mcp` で待ち受けます。待ち受けるアドレスの既定は `127.0.0.1:8000` で、`--address` で変えられます。始めると、待ち受け先を stderr に出します。

```
$ mdya mcp --http --address 127.0.0.1:18765
Listening on http://127.0.0.1:18765/mcp
```

HTTP のサーバーは、mdya を動かしている PC の中から使うものです。ループバック以外のホスト名に宛てた要求は断ります。`Origin` のヘッダーがある要求は、そのスキームが `http` か `https` で、ホストがループバックのときだけ受け付け、それ以外は HTTP の 403 で断ります。`Origin` のヘッダーがない要求は、受け付けます。PC の外のサイトのページから、ブラウザが mdya に送る要求を受け付けないためです。

## AI アシスタントに登録する

### Claude Code

stdio で登録するときは、次のとおりです。

```
claude mcp add --transport stdio mdya -- mdya mcp
```

既定のほかの設定とインデックスのディレクトリを使うときは、`mcp` の前に `--data-dir` を書きます。

```
claude mcp add --transport stdio mdya -- mdya --data-dir /Users/me/work-mdya mcp
```

HTTP のサーバーを動かしておき、そこにつなぐときは、次のとおりです。

```
claude mcp add --transport http mdya http://127.0.0.1:8000/mcp
```

### ほかの MCP のクライアント

stdio では、起動するコマンドを `mdya`、引数を `mcp` にします。`--data-dir` などの全体のオプションは、`mcp` の前の引数に入れます。HTTP では、接続先を `http://127.0.0.1:8000/mcp` (`--address` を変えたときは、そのアドレス) にします。

## ツール

各ツールは、結果の構造を表す出力の schema を、ツールの定義の中で示します。成功したときは、その schema に合う構造で返します。

### `list_collections`

入力はありません。結果は、`mdya collection list --json` と同じ構造です ([コマンドの「collection の一覧を見る」](commands.md#collection-の一覧を見る-mdya-collection-list))。collection の説明は、どの collection を探すかを決めるのに使えます。

### `search`

| 入力 | 意味 | 省略したとき |
|---|---|---|
| `query` | 探すもの。空白だけのものは受け付けない | 省略できない |
| `mode` | `fts`、`vector`、`hybrid` のどれか | `hybrid` |
| `collections` | 探す collection の名前の配列 | すべての collection (空の配列も同じ) |
| `limit` | 返すヒットの数の上限。1 以上 9007199254740991 以下 | 20 |
| `unit` | `document` か `chunk` | `document` |

結果は、`mdya search --json` と同じ構造です ([コマンドの「検索する」](commands.md#検索する-mdya-search))。各ヒットの `collection`、`key`、`chunk_index` を、`get_document` に渡して、文書を読めます。

### `get_document`

| 入力 | 意味 | 省略したとき |
|---|---|---|
| `collection` | collection 名 | 省略できない |
| `key` | 文書の key | 省略できない |
| `chunk_index` | 始まりの chunk の番号 | 文書の本文の全体を返す |
| `end_chunk_index` | 終わりの chunk の番号 (この番号を含む)。`chunk_index` 以上にする。最後の chunk を超えるなら、文書の本文の終わりまで返す | `chunk_index` の chunk だけを返す |

結果は、`collection`、`key`、`text` (返す文字) を持ちます。返す文字が、MCP の上限 (`get.mcp_limit_bytes`、既定は 1 MiB。[設定](configuration.md#文書を取得するときの大きさの上限)) を超えるときは、エラーになります。chunk の範囲を指定して、分けて取得してください。

```json
{"collection":"notes","key":"release.md","text":"# リリースの手順\n\nタグを打つ前に、変更履歴を書き、バージョン番号を上げます。\n\n"}
```

### `get_version`

入力はありません。結果は `{"version":"mdya X.Y.Z"}` の形です。MCP のサーバーとして動いている mdya のバージョンを返します。

#### バージョンを示さない

`mdya config set mcp.show_version false` にすると、MCP で mdya のバージョンを示しません。`get_version` の結果は `{"version":"mdya"}` になり、MCP の初期化のときに返すサーバーの情報の `version` も `mdya` になります。CLI の `mdya --version` と `mdya status` には効きません。

## エラー

ツールのエラーは、MCP のツールの実行のエラー (`isError: true`) として返します。中身は、次の形の JSON のオブジェクトです。同じ中身を、文字としても添えます。

```json
{"code":"collection_not_found","message":"Error: the collection \"nosuch\" does not exist\n\nHow to fix:\n    Check the collection name in the list of collections (mdya collection list, or the list_collections tool).","details":{"collection":"nosuch"}}
```

- `code`: エラーの種類です。下の表のどれかです。知らない `code` は、`internal` と同じに扱ってください。
- `message`: CLI で同じエラーが起きたときに stderr に出すのと同じ説明 (エラーメッセージ、原因、直し方) です。
- `details`: 入力を直して呼び直すのに使う情報です。

| `code` | 当たるエラー | `details` |
|---|---|---|
| `invalid_input` | [クエリが空](errors.md#クエリが空)、[件数の上限が 0](errors.md#件数の上限が-0)、[chunk がない](errors.md#chunk-がない)、[chunk の範囲の指定が正しくない](errors.md#chunk-の範囲の指定が正しくない)、[クエリが、埋め込みモデルのコンテキスト長を超える](errors.md#クエリが埋め込みモデルのコンテキスト長を超える)、入力の値が入力の schema に合わない (知らない検索方式、必須の入力がない、など) | `input`: 正しくない入力の名前 |
| `collection_not_found` | [collection がない](errors.md#collection-がない) | `collection`: collection 名 |
| `document_not_found` | [文書がない](errors.md#文書がない) | `collection`: collection 名、`key`: key |
| `output_too_large` | [出力が大きすぎる](errors.md#出力が大きすぎる) | `size_bytes`: 返すものの大きさ、`limit_bytes`: MCP の上限 |
| `embedding_model_unavailable` | [埋め込みモデルを使えない](errors.md#埋め込みモデルを使えない) | なし (空のオブジェクト) |
| `internal` | [インデックスや設定を読めない](errors.md#インデックスや設定を読めない)、[想定外の失敗](errors.md#想定外の失敗) | なし (空のオブジェクト) |

## 設定を変えたとき

MCP のサーバーは、サーバーを始めたときの設定を使い続けます。`mdya config set` で変えた設定は、サーバーを始め直したときに効きます。

埋め込みモデルの設定を変えたときは、サーバーを始め直してください。始め直さないと、サーバーは前の埋め込みモデルでクエリの埋め込みベクトルを作り、CLI で埋め込みベクトルを作り直したあとは、ベクトル検索で何もヒットしなくなります ([設定と違う埋め込みモデルで作った埋め込みベクトルがある](errors.md#設定と違う埋め込みモデルで作った埋め込みベクトルがある))。

## ログ

MCP のサーバーのログは、stderr に出ます。ログをファイルに残したいときは、シェルで stderr をファイルに振り向けてください ([設定の「ログと色」](configuration.md#ログと色))。
