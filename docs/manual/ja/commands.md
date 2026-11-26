# コマンド

mdya の CLI のコマンドの書き方と、出力です。例の出力の場所は `/Users/me` の下に、mdya のバージョンは `X.Y.Z` に置き換えています。

## コマンドの一覧

| すること | コマンド |
|---|---|
| collection を作る | [`mdya collection add`](#collection-を作る-mdya-collection-add) |
| collection を削除する | [`mdya collection remove`](#collection-を削除する-mdya-collection-remove) |
| collection の一覧を見る | [`mdya collection list`](#collection-の一覧を見る-mdya-collection-list) |
| collection の場所や説明を変える | [`mdya collection set`](#collection-の場所や説明を変える-mdya-collection-set) |
| 指定した collection を update する | [`mdya update <collection 名>`](#collection-を-update-する-mdya-update) |
| すべての collection を update する | [`mdya update --all`](#collection-を-update-する-mdya-update) |
| 文書の内容を渡して取り込む | [`mdya put`](#文書の内容を渡して取り込む-mdya-put) |
| 文書を指定して削除する | [`mdya remove`](#文書を指定して削除する-mdya-remove) |
| 検索する | [`mdya search`](#検索する-mdya-search) |
| 文書を取得する | [`mdya get`](#文書を取得する-mdya-get) |
| インデックスの状態を見る | [`mdya status`](#インデックスの状態を見る-mdya-status) |
| 設定を見る | [`mdya config list`](#設定を見る-mdya-config-list) |
| 設定を変える | [`mdya config set`、`mdya config reset`](#設定を変える-mdya-config-setmdya-config-reset) |
| 埋め込みベクトルを作り直す | [`mdya embedding rebuild`](#埋め込みベクトルを作り直す-mdya-embedding-rebuild) |
| インデックスを保守する | [`mdya optimize`](#インデックスを保守する-mdya-optimize) |
| MCP のサーバーを始める | [`mdya mcp`](#mcp-のサーバーを始める-mdya-mcp) |
| mdya のバージョンを見る | [`mdya --version`、`mdya -v`](#mdya-のバージョンを見る-mdya---versionmdya--v) |
| ヘルプを見る | [`--help`、`-h`](#ヘルプを見る---help-h) |

## どのコマンドにも共通のこと

### 全体のオプション

次のオプションは、`mdya` のすぐあと、コマンドの名前より前に書きます (`mdya --data-dir ~/work-mdya search "手順"`)。コマンドの名前のあとに書くと、引数の使い方のエラー (終了コード 2) になります。

| オプション | 意味 | 既定 |
|---|---|---|
| `--data-dir <ディレクトリ>` | 設定とインデックスのディレクトリ ([設定の「ディレクトリ」](configuration.md#ディレクトリ)) | `~/.mdya` |
| `--model-cache-dir <ディレクトリ>` | 埋め込みモデルのキャッシュのディレクトリ | `~/.mdya-models` |
| `--log-level <ログレベル>` | どのログレベルからログを出すか ([設定の「ログと色」](configuration.md#ログと色)) | `warn` (一部は `error`) |
| `--log-format <形式>` | ログの出力形式 (`compact`、`pretty`、`json`) | `compact` |
| `--no-color` | 出力に色を付けない | 出力先が端末のときだけ色を付ける |

### 出力先

- 標準出力 (stdout) には、コマンドの結果だけを出します。
- 標準エラー出力 (stderr) には、メッセージ (成功したこと、進捗、完了のまとめなど)、エラー、ログを出します。
- `--json` を付けられるコマンドは、結果を JSON で返します。付けなければ、人が読む形で返します。
- エラーのときは、`--json` を付けていても、stdout に何も出しません。stderr に、人が読む形で、エラーメッセージと、その原因と、直し方を出します。スクリプトからは、stdout の中身ではなく、終了コードで成否を見てください。
- 人が読む形で出すものの中の、取り込んだ文書や利用者から来た文字列 (key、スニペット、場所など) にある制御文字 (改行とタブも含む) と、文字の並ぶ向きを変える文字 (U+202A〜U+202E、U+2066〜U+2069) は、見える形に置き換えて出します。端末が、取り込んだものに操られないようにするためです。`mdya get` が返す文書の本文は、置き換えずに、元のまま出します。

### 終了コード

| 終了コード | 意味 |
|---|---|
| 0 | 成功 |
| 1 | エラー |
| 2 | 引数の使い方のエラー |
| 101 | 想定外の失敗 (mdya の不具合) |
| 137 | メモリの上限を超えて止まった |

エラーごとの終了コードと直し方は、[エラーと直し方](errors.md) にあります。

### 進捗

取り込み、埋め込みベクトルの作り直し、保守は、進捗を stderr に出します。stderr が端末のときは、進捗のバーを描き直しながら出します。stderr が端末でないとき (ファイルへの振り向け、パイプなど) と、環境変数 `TERM` が `dumb` のとき (Windows 以外では、`TERM` がないときも) は、始めたときと 10 秒ごとに、状態を 1 行で出します。

```
Updating 0/3, 0 chunks embedded, 10s
```

### 確認を求めるコマンド

`mdya collection remove` と `mdya remove` は、削除する前に、端末で確認を求めます (`[y/N]`)。`y` で削除します。`--yes` を付けると、確認せずに削除します。端末でないところ (スクリプトなど) で流すときは、`--yes` が要ります。

## collection を作る (`mdya collection add`)

```
mdya collection add <collection 名> [--type <collection タイプ>] [--description <説明>] [<場所>]
```

| 引数とオプション | 意味 |
|---|---|
| `<collection 名>` | 作る collection の名前。ASCII の英数字と `-`、`_` だけからなる、1〜64 文字 |
| `--type <collection タイプ>` | `directory` (既定)、`archive`、`raw` のどれか ([collection と collection タイプ](collections.md)) |
| `--description <説明>` | collection の説明。省略できる |
| `<場所>` | ディレクトリの collection ではディレクトリ、アーカイブの collection ではアーカイブのファイル。raw の collection では渡さない |

作るときには、取り込みません。取り込むのは、`mdya update` か `mdya put` です。

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

起こりうるエラー: [同じ名前の collection がある](errors.md#同じ名前の-collection-がある)、[collection 名が決まりに合わない](errors.md#collection-名が決まりに合わない)、[タイプ別の属性が足りないか余計](errors.md#タイプ別の属性が足りないか余計)、[場所が見つからない](errors.md#場所が見つからない)、[場所が collection タイプに合わない](errors.md#場所が-collection-タイプに合わない)

## collection を削除する (`mdya collection remove`)

```
mdya collection remove <collection 名> [--yes] [--no-optimize]
```

collection と、その collection の文書を、インデックスから削除します。取り込み元 (ディレクトリやアーカイブのファイル) には触れません。削除する前に確認を求めます。`--no-optimize` は、[インデックスを保守する](#インデックスを保守する-mdya-optimize) を参照してください。

```
$ mdya collection remove notes-zip --yes
Removed the collection "notes-zip".
```

raw の collection の文書は、update で取り込み直せません。削除したあとに戻すには、同じ文書の内容を、もう一度 `mdya put` で渡す必要があります。

起こりうるエラー: [collection がない](errors.md#collection-がない)、[確認が得られない](errors.md#確認が得られない)

## collection の一覧を見る (`mdya collection list`)

```
mdya collection list [--json]
```

collection ごとに、collection 名、collection タイプ、文書の数、説明、タイプ別の属性を返します。collection は、名前の順 (文字の Unicode のコードポイントの順) に並べます。

```
$ mdya collection list
notes (directory, 0 documents)
  個人のメモ
  path: /Users/me/notes
```

`--json` を付けたときの形は、次のとおりです。`description` は、説明がないときは `null` です。`attributes` は、ディレクトリとアーカイブの collection では場所 (`path`) を持ち、raw の collection では空のオブジェクトです。

```json
{"collections":[{"name":"notes","type":"directory","description":"個人のメモ","document_count":0,"attributes":{"path":"/Users/me/notes"}}]}
```

## collection の場所や説明を変える (`mdya collection set`)

```
mdya collection set <collection 名> [--description <説明> | --no-description] [<場所>]
```

説明か、取り込む場所を変えます。少なくとも 1 つを渡します。`--no-description` は、説明をなくします。新しい場所は、次に update するときに使います。文書は、このコマンドでは変わりません。

collection 名と collection タイプは変えられません。変えたいときは、削除して作り直してください。

```
$ mdya collection set notes --description "仕事のメモ"
Changed the collection "notes".
  path: /Users/me/notes
  description: 仕事のメモ
```

起こりうるエラー: [collection がない](errors.md#collection-がない)、[変えるものを指定していない](errors.md#変えるものを指定していない)、[タイプ別の属性が足りないか余計](errors.md#タイプ別の属性が足りないか余計)、[場所が見つからない](errors.md#場所が見つからない)、[場所が collection タイプに合わない](errors.md#場所が-collection-タイプに合わない)、[この mdya が知らない collection タイプの collection がある](errors.md#この-mdya-が知らない-collection-タイプの-collection-がある)

## collection を update する (`mdya update`)

```
mdya update <collection 名> [--force] [--no-optimize]
mdya update --all [--force] [--no-optimize]
```

ディレクトリとアーカイブの collection を、取り込み元の今の中身に合わせます ([collection と collection タイプ](collections.md))。`mdya update <collection 名>` は、指定した 1 つの collection を、`mdya update --all` は、ディレクトリとアーカイブのすべての collection を update します。raw の collection は update できません。

| オプション | 意味 |
|---|---|
| `--force` | 変わっていない文書も、取り込み直す |
| `--no-optimize` | 最後にインデックスを保守しない ([インデックスを保守する](#インデックスを保守する-mdya-optimize)) |

変わっていない文書は飛ばすので、2 回目からは、変わった文書だけを取り込みます。初めて埋め込みモデルを使うときは、埋め込みモデルを取得します ([設定の「埋め込みモデル」](configuration.md#埋め込みモデル))。

進捗には、処理を終えた文書の数と、処理する文書の全体の数を出します。進捗のバーには、chunk の数を出さず、行の頭に印を出します。印は、埋め込みベクトルを作り終えた chunk が増えると回ります。1 つの文書の処理に長くかかるあいだも、印が回っていれば進んでいます。状態を 1 行で出すときは、埋め込みベクトルを作り終えた chunk の数も出します。終わると、結果ごとの文書の数を出します。

```
$ mdya update notes
Updated the collection "notes": 3 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed.
$ mdya update --all
Updated 2 collections: 4 added, 0 changed, 2 unchanged, 0 re-ingested, 0 deleted, 0 failed.
```

| 結果 | 意味 |
|---|---|
| `added` | 新しく足した |
| `changed` | 変わったので取り込み直した |
| `unchanged` | 変わっていないので飛ばした |
| `re-ingested` | 変わっていないが、`--force` で取り込み直した |
| `deleted` | 取り込み元からなくなったので削除した |
| `failed` | 読めなかった ([読めない文書がある](errors.md#読めない文書がある)) |

読めない文書や、読めない取り込み元があったときは、ほかの文書や collection を取り込んでから、終了コード 1 で終わります。

エラーで止まったときも、エラーメッセージのあとに、止まるまでの結果ごとの文書の数を出します。止まるまでに書き終えた文書は残るので、どこまで進んだかがわかります。

途中で止めたとき (Ctrl+C など) は、もう一度同じコマンドを流してください。`--force` を付けていなければ、変わっていない文書を飛ばすので、止めたところの続きから進みます。

起こりうるエラー: [collection がない](errors.md#collection-がない)、[update できない collection タイプの collection を指定した](errors.md#update-できない-collection-タイプの-collection-を指定した)、[読めない文書がある](errors.md#読めない文書がある)、[取り込み元を読めない](errors.md#取り込み元を読めない)、[埋め込みモデルを使えない](errors.md#埋め込みモデルを使えない)、[埋め込みベクトルを作り直していない](errors.md#埋め込みベクトルを作り直していない)、[埋め込みモデルのコンテキスト長を超える chunk がある](errors.md#埋め込みモデルのコンテキスト長を超える-chunk-がある)、[この mdya が知らない collection タイプの collection がある](errors.md#この-mdya-が知らない-collection-タイプの-collection-がある)

## 文書の内容を渡して取り込む (`mdya put`)

```
mdya put <collection 名> <key> --file-type <ファイルタイプ> [--force] [--no-optimize] <場所>
```

raw の collection に、文書を 1 つ取り込みます。その key の文書がなければ足し、あれば置き換えます。

| 引数とオプション | 意味 |
|---|---|
| `<collection 名>` | raw の collection の名前 |
| `<key>` | 文書の key。空の文字列を含め、どんな文字列も受け付ける |
| `--file-type <ファイルタイプ>` | Markdown は `text/markdown`、`markdown`、`md` のどれか。PDF は `application/pdf`、`pdf` のどれか。大文字と小文字は区別しない。`text/markdown` に `charset` を付けるときは、`UTF-8` だけを受け付ける |
| `--force` | 文書の本文が前と同じでも、取り込み直す |
| `--no-optimize` | 最後にインデックスを保守しない ([インデックスを保守する](#インデックスを保守する-mdya-optimize)) |
| `<場所>` | 文書の内容を読むファイル。`-` なら標準入力から読む |

```
$ mdya put memo idea.md --file-type md ~/memo-idea.md
Added "idea.md" to the collection "memo".
$ printf '# 買うもの\n\n- 牛乳\n' | mdya put memo shopping.md --file-type text/markdown -
Added "shopping.md" to the collection "memo".
$ mdya put memo idea.md --file-type md ~/memo-idea.md
Skipped "idea.md" in the collection "memo": it has not changed.
```

起こりうるエラー: [collection がない](errors.md#collection-がない)、[文書の内容を渡して取り込めない collection タイプの collection を指定した](errors.md#文書の内容を渡して取り込めない-collection-タイプの-collection-を指定した)、[扱えないファイルタイプを指定した](errors.md#扱えないファイルタイプを指定した)、[読めない文書がある](errors.md#読めない文書がある)、[埋め込みモデルを使えない](errors.md#埋め込みモデルを使えない)、[埋め込みベクトルを作り直していない](errors.md#埋め込みベクトルを作り直していない)、[埋め込みモデルのコンテキスト長を超える chunk がある](errors.md#埋め込みモデルのコンテキスト長を超える-chunk-がある)、[この mdya が知らない collection タイプの collection がある](errors.md#この-mdya-が知らない-collection-タイプの-collection-がある)

## 文書を指定して削除する (`mdya remove`)

```
mdya remove <collection 名> <key> [--yes] [--no-optimize]
```

文書を 1 つ、インデックスから削除します。取り込み元には触れません。削除する前に確認を求めます。

ディレクトリとアーカイブの collection の文書は、取り込み元に残っていれば、次に update したときに、また取り込まれます。

```
$ mdya remove memo shopping.md --yes
Removed "shopping.md" from the collection "memo".
$ mdya remove notes setup.md --yes
Removed "setup.md" from the collection "notes".
If it is still in the collection's source, it comes back at the next update.
```

起こりうるエラー: [collection がない](errors.md#collection-がない)、[文書がない](errors.md#文書がない)、[確認が得られない](errors.md#確認が得られない)、[この mdya が知らない collection タイプの collection がある](errors.md#この-mdya-が知らない-collection-タイプの-collection-がある)

## 検索する (`mdya search`)

```
mdya search <クエリ> [--mode <fts|vector|hybrid>] [-c, --collection <collection 名>]... [-n, --limit <件数の上限>] [--unit <document|chunk>] [--json]
```

| 引数とオプション | 意味 | 既定 |
|---|---|---|
| `<クエリ>` | 探すもの。空白だけのものは受け付けない | なし (必須) |
| `--mode` | 検索方式。`fts` (全文検索)、`vector` (ベクトル検索)、`hybrid` (ハイブリッド検索) | `hybrid` |
| `-c`、`--collection` | 探す collection。カンマで区切るか (`-c notes,work`)、繰り返して (`-c notes -c work`) 複数を渡せる。指定したどれかの collection の文書を探す。`-c notes work` のようにスペースで区切った語は、collection 名として読まない | すべての collection |
| `-n`、`--limit` | 返すヒットの数の上限。1 以上 9007199254740991 以下 | 20 |
| `--unit` | `document` (文書ごとに 1 つのヒット) か `chunk` (chunk ごとに 1 つのヒット) | `document` |
| `--json` | 結果を JSON で返す | |

### 検索方式

- `fts` (全文検索): クエリの語が、文書や chunk に現れるかで探します。埋め込みモデルを使いません。スコアは BM25 の値 (語が現れる回数と、その語の珍しさから求める値) で、大きいほど上に並べます。
- `vector` (ベクトル検索): クエリと chunk の意味の近さで探します。スコアは cosine 類似度で、0 から 1 の値です。速く探すための索引で探すので、最も近い文書や chunk を取りこぼすことがあります。
- `hybrid` (ハイブリッド検索): 全文検索とベクトル検索の結果を、それぞれの順位で合わせます (RRF)。スコアは、それぞれの検索での順位 (0 から数える) を r として、1 / (60 + r) を両方の検索で求めて足した値です。片方の検索でヒットしなかったときは、その側の値を 0 とします。

検索方式によってスコアの意味が違うので、違う検索方式のスコアどうしを比べないでください。

全文検索の語の切り方は、次のとおりです。

- 文章を、日本語の辞書 (IPADIC) で語に切ります。カタカナの複合語は、辞書にある語だけでできていれば、その語に分けます (「モデルキャッシュ」は「モデル」と「キャッシュ」)。
- 英字と数字が混ざる並びは、英字と数字の切り替わりで分けます (`u32` は `u` と `32`)。英字の語の中の記号も語になります (`C++` の `++`)。
- 半角と全角のカナや英数字、丸付きの数字などは、同じ文字として扱います。大文字と小文字、アクセント記号の有無、英語の語形の違い (`run` と `running`) は区別しません。日本語の活用の違い (「走る」と「走った」) は区別します。
- 日本語の助詞、助動詞、接続詞、句読点や括弧などと、英語のよく出る語 (`the`、`a`、`you` など) は、探す語にしません。そのため、否定の「ない」も探す語にせず、「削除しない」は「削除し」と同じに探します。クエリの語が、すべて探す語にしない語のときは、何もヒットしません。
- 255 バイト以上の語 (UTF-8 で数えます。日本語では、およそ 85 字以上) は、語にしません。

### 結果

ヒットを、スコアの高い順に返します。スコアが同じヒットは、collection 名の順、key の順 (どちらも、文字の Unicode のコードポイントの順) に並べ、`--unit chunk` のときは、さらに chunk の番号の小さい順に並べます。

- `--unit document`: ヒットした文書を、件数の上限まで返します。
- `--unit chunk`: ヒットした chunk を、1 つずつ別のヒットとして、件数の上限まで返します。

スニペットは、ヒットした chunk から抜き出した、60 文字までの文字列です。クエリの語のうち、chunk で最初に現れる語の 30 文字前から抜き出します。どの語も現れなければ、chunk の先頭から抜き出します。改行は空白に置き換え、前か後ろを切ったときは `…` を付けます。

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

各ヒットの 1 行目は、collection 名と key です。2 行目は、スコアと、スニペットを抜き出した chunk の番号 (0 から数える) と、その文書の chunk の数です。3 行目がスニペットです。

`--json` を付けたときの形は、次のとおりです (読みやすいように改行を入れています)。

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

- `query`、`mode`、`unit`、`collections`、`limit`: 渡したクエリ、検索方式、`document` か `chunk` か、collection 名、件数の上限です。collection を指定しなかったときは、`collections` は空の配列です。
- `hit_count`: 返したヒットの数です。
- 各ヒットの `chunk_index` はスニペットを抜き出した chunk の番号、`chunk_count` はその文書の chunk の数です。`mdya get` で chunk の範囲を指定するのに使えます。
- `warnings`: 警告の文の配列です。たとえば、設定の埋め込みモデルとインデックスの埋め込みベクトルを作った埋め込みモデルが違うときに入ります ([設定と違う埋め込みモデルで作った埋め込みベクトルがある](errors.md#設定と違う埋め込みモデルで作った埋め込みベクトルがある))。

起こりうるエラー: [クエリが空](errors.md#クエリが空)、[件数の上限が 0](errors.md#件数の上限が-0)、[クエリが、埋め込みモデルのコンテキスト長を超える](errors.md#クエリが埋め込みモデルのコンテキスト長を超える)、[collection がない](errors.md#collection-がない)、[埋め込みモデルを使えない](errors.md#埋め込みモデルを使えない)、[設定と違う埋め込みモデルで作った埋め込みベクトルがある](errors.md#設定と違う埋め込みモデルで作った埋め込みベクトルがある)

## 文書を取得する (`mdya get`)

```
mdya get <collection 名> <key> [--chunk-index <番号> [--end-chunk-index <番号>]] [--no-size-limit]
```

文書の本文を、手を加えずに stdout に出します。末尾に改行も足しません。返すのはインデックスにある文書の本文で、取り込み元を読み直しません。

| オプション | 意味 |
|---|---|
| `--chunk-index <番号>` | 文書の本文の全体ではなく、その番号の chunk の範囲の文字を返す |
| `--end-chunk-index <番号>` | `--chunk-index` の chunk から、この番号の chunk まで (この番号を含む) の範囲の文字を、一続きで返す。`--chunk-index` 以上の番号にする。最後の chunk を超える番号なら、文書の本文の終わりまで返す |
| `--no-size-limit` | 出力の大きさの上限 ([設定の「文書を取得するときの大きさの上限」](configuration.md#文書を取得するときの大きさの上限)) を外す |

chunk の範囲は、文書の本文を隙間なく覆います。隣り合う chunk の範囲が重なることがあります。範囲を 1 回で指定したときは、重なる文字は 1 度だけ入ります。

```
$ mdya get notes release.md --chunk-index 0
# リリースの手順

タグを打つ前に、変更履歴を書き、バージョン番号を上げます。

```

起こりうるエラー: [collection がない](errors.md#collection-がない)、[文書がない](errors.md#文書がない)、[chunk がない](errors.md#chunk-がない)、[chunk の範囲の指定が正しくない](errors.md#chunk-の範囲の指定が正しくない)、[出力が大きすぎる](errors.md#出力が大きすぎる)

## インデックスの状態を見る (`mdya status`)

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

- `configured embedding model`: 設定の埋め込みモデルと、それが作る埋め込みベクトルの次元です。
- `index embedding model`: インデックスの埋め込みベクトルを作った埋め込みモデルと、それが設定の埋め込みモデルと同じかです。その下の行は、その埋め込みモデルで作った埋め込みベクトルを持つ chunk の数と、そのうち埋め込みベクトルが chunk の全体を表していない chunk の数 (`truncated`。[埋め込みモデルのコンテキスト長を超える chunk がある](errors.md#埋め込みモデルのコンテキスト長を超える-chunk-がある)) です。インデックスに chunk がないときは、`index embedding model: none` と出ます。
- `chunks without embeddings`: 埋め込みベクトルを持たない chunk の数です。
- `collections`、`documents`、`chunks`: インデックス全体の collection、文書、chunk の数です。
- `warning`: 結果の警告です。設定の埋め込みモデルを使えずに次元を確かめられなかったときに、最後に、そのことと理由を 1 行で出します ([埋め込みモデルを使えない](errors.md#埋め込みモデルを使えない))。`--json` では `warnings` に入ります。

`--json` を付けたときの形は、次のとおりです。

```json
{"version":"X.Y.Z","configured_embedding_model":"cl-nagoya/ruri-v3-30m","configured_embedding_dimension":256,"index_embedding_models":[{"model":"cl-nagoya/ruri-v3-30m","dimension":256,"configured":true,"chunk_count":7,"truncated_chunk_count":0}],"chunks_without_embedding_count":0,"collection_count":1,"document_count":3,"chunk_count":7,"warnings":[]}
```

`configured_embedding_dimension` と `configured` は、設定の埋め込みモデルを使えずに次元を確かめられなかったときは `null` です。そのときは、理由を `warnings` に入れます ([埋め込みモデルを使えない](errors.md#埋め込みモデルを使えない))。エラーにはしません。

## 設定を見る (`mdya config list`)

```
mdya config list [--json]
```

設定の名前と、今の値を返します。変えていない設定は、既定の値を返します。各設定の意味は、[設定](configuration.md) にあります。

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

`--json` を付けると、設定の名前を項目の名前にした JSON のオブジェクトで返します。

```json
{"mcp.show_version":true,"embedding.model":"cl-nagoya/ruri-v3-30m","embedding.ollama_endpoint":"http://127.0.0.1:11434","resources.memory_limit_mb":8192,"resources.embedding_parallelism":8,"get.cli_limit_bytes":1048576,"get.mcp_limit_bytes":1048576}
```

## 設定を変える (`mdya config set`、`mdya config reset`)

```
mdya config set <設定の名前> <値>
mdya config reset <設定の名前>
```

`mdya config set` は設定を 1 つ変え、`mdya config reset` は既定の値に戻します。設定の名前と、受け付ける値は、[設定](configuration.md) にあります。設定のファイルを直接書き換えないでください。直接書き換えたときの動作は、約束していません。

```
$ mdya config set resources.embedding_parallelism 4
Changed resources.embedding_parallelism from 8 to 4.
$ mdya config reset resources.embedding_parallelism
Changed resources.embedding_parallelism from 4 to 8.
```

埋め込みモデルを変えたときは、インデックスの埋め込みベクトルを作った埋め込みモデルが変えたあとの設定と違えば、そのことを伝えます。`mdya embedding rebuild` で作り直すまで、ベクトル検索は何もヒットせず、`mdya update` と `mdya put` はエラーになります ([設定の「埋め込みモデルを変える」](configuration.md#埋め込みモデルを変える))。

```
$ mdya config set embedding.model sentence-transformers/all-MiniLM-L6-v2
Changed embedding.model from cl-nagoya/ruri-v3-30m to sentence-transformers/all-MiniLM-L6-v2.
The index's embeddings were made with another model (cl-nagoya/ruri-v3-30m).
Until you run mdya embedding rebuild, vector search finds nothing, and update and put fail.
```

変えたあとの埋め込みモデルの名前が、インデックスの埋め込みベクトルを作った埋め込みモデルと同じで、そのモデルを使えずに次元を確かめられないときは、エラーにせず設定を変えます。インデックスの埋め込みベクトルが、変えたあとの埋め込みモデルで作ったものかがわからないことと、その理由を伝えます ([埋め込みモデルを使えない](errors.md#埋め込みモデルを使えない))。

動いている MCP のサーバーには、サーバーを始め直したときに効きます。

起こりうるエラー: [設定の名前がない](errors.md#設定の名前がない)、[設定の値が正しくない](errors.md#設定の値が正しくない)

## 埋め込みベクトルを作り直す (`mdya embedding rebuild`)

```
mdya embedding rebuild [--force] [--no-optimize]
```

インデックスの埋め込みベクトルを、設定の埋め込みモデルで作り直します。取り込み元は読まず、文書の本文と chunk は変えません。

- インデックスの埋め込みベクトルを作った埋め込みモデルが、設定と同じときは、何もしません。
- `--force` を付けると、同じときも作り直します。Ollama のモデルを、同じ名前のまま、埋め込みベクトルの次元も同じモデルに取り替えたときに使います。mdya は、埋め込みモデルを、名前と、埋め込みベクトルの次元で区別するので、取り替えたことを見分けられないためです。次元の違うモデルに取り替えたときは、`--force` がなくても作り直します。
- 作り直しているあいだは、前の埋め込みベクトルを残し、すべての chunk を作り直し終えたときに、新しい埋め込みベクトルに切り替えます。埋め込みモデルを変えたあとの作り直しでは、前の埋め込みベクトルは設定と違うので、CLI の検索と、設定を変えたあとに始めた MCP のサーバーでは、切り替えるまで、ベクトル検索は何もヒットしません。設定を変える前に始めた MCP のサーバーは、サーバーを始めたときの設定で比べるので、切り替えるまではヒットし、切り替えたあとはヒットしなくなります ([MCP のサーバーの「設定を変えたとき」](mcp.md#設定を変えたとき))。`--force` で同じ埋め込みモデルのまま作り直すときは、切り替えるまで、前の埋め込みベクトルで検索します。
- 途中で止めたときは、作りかけの埋め込みベクトルを検索に使いません。同じコマンドをもう一度流すと、作り直しを終えます。

```
$ mdya embedding rebuild
Rebuilt the embeddings of 15 chunks.
$ mdya embedding rebuild
Nothing was rebuilt: the embeddings were already made with the configured model.
```

起こりうるエラー: [埋め込みモデルを使えない](errors.md#埋め込みモデルを使えない)、[埋め込みモデルのコンテキスト長を超える chunk がある](errors.md#埋め込みモデルのコンテキスト長を超える-chunk-がある)

## インデックスを保守する (`mdya optimize`)

```
mdya optimize
```

インデックスは、書き込むたびに、書き込む前の状態を残し、ファイルも細かく増えます。保守では、細かいファイルをまとめて検索を速く保ち、書き込む前の状態のうち、作ってから 7 日より前のものを消して、ディスクの使用量を抑えます。

次のコマンドも、既定で、最後に保守します。`--no-optimize` を付けると、保守しません。そのときは、あとで `mdya optimize` で保守できます。

- `mdya collection remove`
- `mdya update`
- `mdya put`
- `mdya remove`
- `mdya embedding rebuild`

```
$ mdya optimize
Maintained the index.
```

7 日より長く書き込みのなかったインデックスを保守すると、別のプロセス (MCP のサーバーなど) で、保守の前に始めた検索が、エラー ([インデックスや設定を読めない](errors.md#インデックスや設定を読めない)) になることがあります。もう一度検索すれば、読めます。`--no-optimize` を付けた書き込みでは、保守に時間がかからず、このエラーも起きません。

書き込みのコマンドの最後の保守に失敗したときは、コマンドを終えたままにし、保守に失敗したことを警告として出します。終了コードは、保守の失敗では変わりません。`mdya optimize` で保守に失敗したときは、エラーで終わります ([インデックスや設定に書き込めない](errors.md#インデックスや設定に書き込めない))。

## MCP のサーバーを始める (`mdya mcp`)

```
mdya mcp [--http] [--address <アドレス>]
```

AI アシスタントなどの MCP のクライアントから使う MCP のサーバーを始めます。使い方は、[MCP のサーバー](mcp.md) にあります。

| オプション | 意味 | 既定 |
|---|---|---|
| `--http` | stdio ではなく、HTTP で待ち受ける | stdio |
| `--address <アドレス>` | HTTP で待ち受けるアドレス。`<ホスト>:<番号>` の形。IPv6 のアドレスは `[ ]` で囲む | `127.0.0.1:8000` |

起こりうるエラー: [待ち受けるアドレスを使えない](errors.md#待ち受けるアドレスを使えない)、[MCP のクライアントとのやりとりを始められない](errors.md#mcp-のクライアントとのやりとりを始められない)

## mdya のバージョンを見る (`mdya --version`、`mdya -v`)

```
$ mdya --version
mdya X.Y.Z
```

`mdya -v` も、`mdya --version` と同じです。

## ヘルプを見る (`--help`、`-h`)

どのコマンドにも、`--help` か `-h` を付けられます。`mdya --help` はコマンドの一覧を、`mdya collection --help` は `collection` の下のコマンドの一覧を、`mdya collection add --help` は、そのコマンドの引数とその説明を、stdout に出します。

`--help` があるときは、ほかの引数を確かめずにヘルプを出します (`mdya search --help` はクエリがなくてもヘルプを出します)。コマンドの名前が正しくないとき (`mdya nosuchcmd --help`) は、引数の使い方のエラー (終了コード 2) です。

引数のない `mdya` は、`mdya --help` と同じです。下のコマンドが足りないとき (`mdya collection`) は、ヘルプを出さず、引数の使い方のエラー (終了コード 2) になります。
