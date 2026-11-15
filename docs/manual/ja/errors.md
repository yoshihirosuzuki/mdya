# エラーと直し方

mdya がエラーで終わったときに、何が起きたかと、どう直すかを引くための説明です。

エラーのとき、mdya は stderr に、`Error:` で始まるエラーメッセージ、その原因 (`Caused by:`)、直し方 (`How to fix:`) を出します。MCP のツールのエラーでは、同じ説明が `message` に入ります ([MCP のサーバーの「エラー」](mcp.md#エラー))。

```
Error: the collection "nosuch" does not exist

How to fix:
    Check the collection name in the list of collections (mdya collection list, or the list_collections tool).
```

## 終了コード

| 終了コード | 意味 |
|---|---|
| 0 | 成功 |
| 1 | エラー。下の表のエラーメッセージで、どの節かを引いてください |
| 2 | 引数の使い方のエラー。渡した引数だけでわかるもの |
| 101 | [想定外の失敗](#想定外の失敗) |
| 137 | [メモリの上限を超えた](#メモリの上限を超えた) |
| OS が決める値 | [中断](#中断) (Ctrl+C など) |

## エラーメッセージから引く

| エラーメッセージ (`Error:` のあと) | 終了コード | 節 |
|---|---|---|
| `a collection named "…" already exists` | 1 | [同じ名前の collection がある](#同じ名前の-collection-がある) |
| `the collection name "…" does not follow the naming rules` | 2 | [collection 名が決まりに合わない](#collection-名が決まりに合わない) |
| `the type-specific attributes are missing or unexpected …` | 2、1 | [タイプ別の属性が足りないか余計](#タイプ別の属性が足りないか余計) |
| `the location was not found` | 1 | [場所が見つからない](#場所が見つからない) |
| `the location does not fit the collection type` | 1 | [場所が collection タイプに合わない](#場所が-collection-タイプに合わない) |
| `the collection "…" does not exist` | 1 | [collection がない](#collection-がない) |
| `nothing to change was given` | 2 | [変えるものを指定していない](#変えるものを指定していない) |
| `the collection "…" is of a type that cannot be updated` | 1 | [update できない collection タイプの collection を指定した](#update-できない-collection-タイプの-collection-を指定した) |
| `the collection "…" is of a type that does not take document content` | 1 | [文書の内容を渡して取り込めない collection タイプの collection を指定した](#文書の内容を渡して取り込めない-collection-タイプの-collection-を指定した) |
| `the file type "…" is not supported` | 2 | [扱えないファイルタイプを指定した](#扱えないファイルタイプを指定した) |
| `the document "…" does not exist in the collection "…"` | 1 | [文書がない](#文書がない) |
| `the query is empty` | 2 | [クエリが空](#クエリが空) |
| `the limit is zero` | 2 | [件数の上限が 0](#件数の上限が-0) |
| `the chunk does not exist` | 1 | [chunk がない](#chunk-がない) |
| `the chunk range is not valid` | 2 | [chunk の範囲の指定が正しくない](#chunk-の範囲の指定が正しくない) |
| `there is no setting named "…"` | 2 | [設定の名前がない](#設定の名前がない) |
| `the value is not valid for the setting …` | 2 | [設定の値が正しくない](#設定の値が正しくない) |
| `a confirmation is needed, but there is no terminal to ask in and --yes was not given` | 2 | [確認が得られない](#確認が得られない) |
| `the document's body is too large to return: …` (`the chunk is …`、`the chunk range is …` も) | 1 | [出力が大きすぎる](#出力が大きすぎる) |
| `the document content cannot be read`、`the document's body cannot be extracted` | 1 | [読めない文書がある](#読めない文書がある) |
| `the embedding model cannot be used` | 1 | [埋め込みモデルを使えない](#埋め込みモデルを使えない) |
| `the configured embedding model … differs from the one the index's embeddings were made with, …` | 1 | [埋め込みベクトルを作り直していない](#埋め込みベクトルを作り直していない) |
| `a rebuild of the embeddings was stopped while switching over to the new embedding model` | 1 | [埋め込みベクトルを作り直していない](#埋め込みベクトルを作り直していない) |
| `the query is longer than the embedding model's context length` | 1 | [クエリが、埋め込みモデルのコンテキスト長を超える](#クエリが埋め込みモデルのコンテキスト長を超える) |
| `the index cannot be read`、`the settings cannot be read`、`the collection information cannot be read` | 1 | [インデックスや設定を読めない](#インデックスや設定を読めない) |
| `the index cannot be written`、`the settings cannot be written`、`the collection information cannot be written`、`the write lock cannot be taken` | 1 | [インデックスや設定に書き込めない](#インデックスや設定に書き込めない) |
| `another write operation is in progress` | 1 | [ほかの書き込み操作が進んでいる](#ほかの書き込み操作が進んでいる) |
| `mdya's memory usage went over the limit of … MB` | 137 | [メモリの上限を超えた](#メモリの上限を超えた) |
| `cannot listen on …` | 1 | [待ち受けるアドレスを使えない](#待ち受けるアドレスを使えない) |
| `the connection with the MCP client cannot be started` | 1 | [MCP のクライアントとのやりとりを始められない](#mcp-のクライアントとのやりとりを始められない) |
| `the settings and index directory cannot be determined` | 1 | [設定とインデックスのディレクトリを決められない](#設定とインデックスのディレクトリを決められない) |
| `this version of mdya does not know the collection type "…" of the collection "…"` | 1 | [この mdya が知らない collection タイプの collection がある](#この-mdya-が知らない-collection-タイプの-collection-がある) |
| `an unexpected failure occurred, which is a bug in mdya` | 101 | [想定外の失敗](#想定外の失敗) |
| `the arguments are not valid: …` | 2 | [引数の使い方のエラー](#引数の使い方のエラー) |
| `the tool input … is not valid: …` | (MCP だけ) | [MCP のサーバーの「エラー」](mcp.md#エラー) の `invalid_input` |

`Error:` の行がなく、update が終了コード 1 で終わったときは、[読めない文書がある](#読めない文書がある) か [取り込み元を読めない](#取り込み元を読めない) です。stderr の警告 (`WARN`) を見てください。

## 指定が正しくない

渡したものが正しくないエラーです。どれも、何もせずに終わり、mdya の中身は変わりません。

### 同じ名前の collection がある

- 起きるとき: `mdya collection add` で、同じ名前の collection がすでにあるとき。終了コード 1。
- 直し方: 別の collection 名にします。

### collection 名が決まりに合わない

- 起きるとき: `mdya collection add` で、collection 名が決まり (ASCII の英数字と `-`、`_` だけからなる、1〜64 文字) に合わないとき。終了コード 2。
- 直し方: 決まりに合う collection 名にします。

### タイプ別の属性が足りないか余計

- 起きるとき: ディレクトリやアーカイブの collection に場所を渡さなかったとき、raw の collection に場所を渡したとき。`mdya collection add` では終了コード 2、`mdya collection set` では終了コード 1 (すでにある collection のタイプを見ないとわからないため)。
- 直し方: ディレクトリとアーカイブの collection には場所を渡し、raw の collection には渡しません。

### 場所が見つからない

- 起きるとき: `mdya collection add` と `mdya collection set` で、渡した場所がないとき。場所がディレクトリかファイルかを、権限がないなどで読めないときと、場所を絶対パスにできないとき (ホームディレクトリがわからないときの `~`、作業ディレクトリを読めないときの相対パス) も含みます。原因 (`Caused by:`) に、読めなかった理由が出ます。終了コード 1。
- 直し方: ある場所を渡します。読めなかったときは、mdya が読めるように権限を直すか、読める場所を渡します。絶対パスにできなかったときは、絶対パスを渡すか、読める作業ディレクトリに移ってやり直します。

### 場所が collection タイプに合わない

- 起きるとき: ディレクトリの collection にファイルを渡したとき、アーカイブの collection に、ディレクトリや、扱わない形式のファイルを渡したとき。終了コード 1。
- 直し方: ディレクトリの collection にはディレクトリを、アーカイブの collection には `.zip`、`.tar`、`.tar.gz`、`.tgz` のファイルを渡します。

### collection がない

- 起きるとき: collection を指定するコマンドとツールで、その名前の collection がないとき。終了コード 1。MCP の `code` は `collection_not_found`。
- 直し方: `mdya collection list` (MCP では `list_collections`) で、collection 名を確かめます。

### 変えるものを指定していない

- 起きるとき: `mdya collection set` に、説明も場所も渡さなかったとき。終了コード 2。
- 直し方: `--description`、`--no-description`、場所のどれかを渡します。

### update できない collection タイプの collection を指定した

- 起きるとき: `mdya update` に、raw の collection を指定したとき。終了コード 1。
- 直し方: raw の collection の文書は、`mdya put` で変えます。

### 文書の内容を渡して取り込めない collection タイプの collection を指定した

- 起きるとき: `mdya put` に、ディレクトリかアーカイブの collection を指定したとき。終了コード 1。
- 直し方: ディレクトリとアーカイブの collection の文書は、取り込み元を変えて、`mdya update` で取り込みます。

### 扱えないファイルタイプを指定した

- 起きるとき: `mdya put` の `--file-type` に、Markdown と PDF のほかを指定したとき。終了コード 2。
- 直し方: Markdown は `text/markdown` (`markdown`、`md`)、PDF は `application/pdf` (`pdf`) で指定します。

### 文書がない

- 起きるとき: `mdya remove`、`mdya get`、MCP の `get_document` で、その key の文書がないとき。終了コード 1。MCP の `code` は `document_not_found`。
- 直し方: 検索の結果の key などで、key を確かめます。

### クエリが空

- 起きるとき: 検索で、クエリが空白だけのとき。終了コード 2。MCP の `code` は `invalid_input`。
- 直し方: 空白だけでないクエリを渡します。

### 件数の上限が 0

- 起きるとき: 検索で、件数の上限に 0 を渡したとき。終了コード 2。MCP の `code` は `invalid_input`。
- 直し方: 1 以上の件数の上限を渡します。

### chunk がない

- 起きるとき: 文書の取得で、始まりの chunk の番号が、その文書の chunk の数以上のとき。終了コード 1。MCP の `code` は `invalid_input`。
- 直し方: 文書の chunk の数 (検索の結果の `chunk_count`) より小さい番号を、始まりに指定します。

### chunk の範囲の指定が正しくない

- 起きるとき: 文書の取得で、始まりの chunk の番号なしに終わりの番号を指定したとき、終わりが始まりより小さいとき。終了コード 2。MCP の `code` は `invalid_input`。
- 直し方: 始まりを指定し、終わりは始まり以上にします。

### 設定の名前がない

- 起きるとき: `mdya config set` と `mdya config reset` で、その名前の設定がないとき。終了コード 2。
- 直し方: `mdya config list` で、設定の名前を確かめます。

### 設定の値が正しくない

- 起きるとき: `mdya config set` で、その設定が受け付けない値を渡したとき。終了コード 2。
- 直し方: その設定が受け付ける値にします ([設定](configuration.md#設定の変え方))。直し方 (`How to fix:`) にも、受け付ける値が出ます。

## 確認が得られない

- 起きるとき: `mdya collection remove` と `mdya remove` で、確認に `y` で答えなかったとき (終了コード 1。`Did not remove …` と出ます)、または、端末でないところで `--yes` を付けなかったとき (終了コード 2)。
- 残るもの: 何も削除しません。
- 直し方: 端末で確認に答えるか、`--yes` を付けます。

## 出力が大きすぎる

- 起きるとき: 文書の取得で、返す文字の大きさが、出力の大きさの上限 ([設定](configuration.md#文書を取得するときの大きさの上限)) を超えるとき。終了コード 1。MCP の `code` は `output_too_large`。
- 残るもの: 何も返しません。
- 直し方: chunk の範囲を指定して、分けて取得します。CLI では、`--no-size-limit` で上限を外して取得することもできます。

```
Error: the document's body is too large to return: 413 bytes, over the limit of 10 bytes

How to fix:
    Get the document in parts by giving a range of chunks.
    On the command line, you can also remove the limit with --no-size-limit.
```

## 読めない文書がある

- 起きるとき: 取り込みで、文書の内容を読めないか、文書の本文を取り出せないとき。たとえば、ファイルを読む権限がない、PDF が壊れている、Markdown の文書の内容が UTF-8 でない、ファイルの相対パスを UTF-8 として読めない、アーカイブの中の項目が読めない項目に当たる ([collection と collection タイプの「読めない文書になる項目」](collections.md#読めない文書になる項目))、などです。
- update のとき: 読めない文書を飛ばし、ほかの文書は取り込んで、終了コード 1 で終わります。読めなかった文書ごとに、key と理由を、警告として出します。完了のまとめでは `failed` に数えます。読めなかった文書は、前に取り込んだ文書のバージョンがあれば、そのまま残ります。
- `mdya put` のとき: 何もせずに、終了コード 1 で終わります。

```
WARN mdya::ingest::flow: a document cannot be read and was skipped collection="broken" key="bad.md" cause="the document's body cannot be extracted: invalid utf-8 sequence of 1 bytes from index 0"
Updated the collection "broken": 1 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 1 failed.
```

- 直し方: その文書を読めるようにして (権限を直す、壊れていないファイルにする、ファイルとディレクトリの名前を UTF-8 にする、アーカイブを読めない項目に当たらないように作り直す、など)、もう一度取り込みます。

警告はログなので、`--log-level error` では出ません。

## 取り込み元を読めない

- 起きるとき: update で、collection の取り込み元そのものを読めないとき。ディレクトリの collection では、ディレクトリがないか、読む権限がないとき。アーカイブの collection では、アーカイブの全体を読めないとき ([collection と collection タイプの「アーカイブ全体を読めないとき」](collections.md#アーカイブ全体を読めないとき))。ディレクトリがあって中が空のときは、これに当たらず、その collection の文書はすべて削除されます。
- 動作: その collection を飛ばし、`mdya update --all` では、ほかの collection を続けます。最後は終了コード 1 で終わります。読めなかった collection ごとに、collection 名と理由を、警告として出します。
- 残るもの: 読めなかった collection の文書は、インデックスにそのまま残ります。

```
WARN mdya::archive_ingest: the source cannot be read and the collection was not updated collection="broken-zip" cause="no valid end of central directory record"
Updated the collection "broken-zip": 0 added, 0 changed, 0 unchanged, 0 re-ingested, 0 deleted, 0 failed.
```

- 直し方: 取り込み元を読めるようにして (ディスクをつなぐ、場所を直す、など)、もう一度 update します。取り込み元の場所が変わったときは、`mdya collection set` で場所を変えます。

ディレクトリの collection で、ディレクトリそのものは読めて、中の一部だけを読めないとき (読む権限のないサブディレクトリがある、外付けのディスクが途中で外れた、など) は、次のとおりです。

- 読めたファイルは取り込みます。削除するのは、ないと確かめられた文書だけです。読めなかったディレクトリの中にあった文書は、削除しません。読めなかったところがわからないときは、その collection では、文書を 1 つも削除しません。
- 読めなかったところごとに、場所と理由を、警告として出し、最後は終了コード 1 で終わります。
- 直し方: 読めなかったところを読めるようにして、もう一度 update します。

## 埋め込みモデルを使えない

- 起きるとき: 埋め込みモデルを使う必要があるのに、使えないとき。たとえば、初めて使うモデルを取得しようとしてネットワークにつながらない、許可の要るモデルで `HF_TOKEN` がない、埋め込みモデルのキャッシュのディレクトリを決められない、Ollama に任せているのに Ollama が動いていない、などです。原因 (`Caused by:`) に、使えない理由が出ます。
- 埋め込みベクトルを作るとき (ベクトル検索、ハイブリッド検索、update、`mdya put`、`mdya embedding rebuild`): 終了コード 1 で終わります。MCP の `code` は `embedding_model_unavailable`。update の途中で使えなくなったときは、そこで止め、それまでに取り込んだ文書は残します。全文検索と文書の取得は、そのまま使えます。
- 埋め込みベクトルの次元を確かめるだけのとき (`mdya status`、埋め込みモデルの設定を変えるとき、埋め込みベクトルを作る文書がなかった update と `mdya put`): エラーにせず、続けます。次元を確かめられなかったことと理由を、`mdya status` では結果の警告に、設定を変えるときはメッセージに、update と `mdya put` では警告に出します。ただし、インデックスに埋め込みベクトルが 1 つもないときは、update と `mdya put` が、埋め込みベクトルを作る文書がなくても、埋め込みベクトルを作るときと同じく、終了コード 1 で終わることがあります。
- 直し方: 理由を取り除いて (ネットワークにつなぐ、`HF_TOKEN` を設定する、`--model-cache-dir` でキャッシュのディレクトリの絶対パスを渡す、Ollama を動かす、など)、もう一度その操作をします。MCP のサーバーでは、`--model-cache-dir` を渡し直すときは、サーバーを始め直します。

## 設定と違う埋め込みモデルで作った埋め込みベクトルがある

- 起きるとき: ベクトル検索とハイブリッド検索で、インデックスの埋め込みベクトルを作った埋め込みモデルが、設定の埋め込みモデルと違うとき。たとえば、埋め込みモデルの設定を変えて、まだ `mdya embedding rebuild` をしていないときです。動いている MCP のサーバーでは、サーバーを始めたときの設定と比べます。
- 動作: エラーにせず、続けます。ベクトル検索は何もヒットしません。ハイブリッド検索は、全文検索の側だけで並べます。警告を stderr に出し、`--json` と MCP では、結果の `warnings` にも入れます。

```
WARN mdya::query::search: The index's embeddings were made with cl-nagoya/ruri-v3-30m (256 dimensions), but the query's embedding was made with sentence-transformers/all-MiniLM-L6-v2 (384 dimensions). Vector search finds nothing until you run mdya embedding rebuild, or restart the MCP server if the model setting changed after it started.
```

- 直し方: `mdya embedding rebuild` で、設定の埋め込みモデルで作り直します。動いている MCP のサーバーで、サーバーを始めたあとに埋め込みモデルの設定を変えたときは、サーバーを始め直します。

## 埋め込みベクトルを作り直していない

- 起きるとき: update と `mdya put` で、インデックスの埋め込みベクトルを作った埋め込みモデルが、設定の埋め込みモデルと違うとき。インデックスに chunk がまだないときは、当たりません。埋め込みベクトルの作り直しを、新しい埋め込みベクトルに切り替える途中で止めたあとにも、当たることがあります。そのときのエラーメッセージは `a rebuild of the embeddings was stopped while switching over to the new embedding model` です。終了コード 1。
- 残るもの: 文書を書かずに終わるので、変わりません。違う埋め込みモデルの埋め込みベクトルを混ぜないためです。

```
Error: the configured embedding model sentence-transformers/all-MiniLM-L6-v2 (384 dimensions) differs from the one the index's embeddings were made with, cl-nagoya/ruri-v3-30m (256 dimensions)

How to fix:
    Rebuild the embeddings with mdya embedding rebuild, then run the operation again.
```

- 直し方: `mdya embedding rebuild` をしてから、もう一度取り込みます。

## 埋め込みモデルのコンテキスト長を超える chunk がある

埋め込みモデルは、一度に受け取れる文章の長さ (コンテキスト長) が決まっています。chunk がそれを超えると、その chunk の埋め込みベクトルは、chunk の先頭から、コンテキスト長に収まる部分だけを表します。

- 起きるとき: update、`mdya put`、`mdya embedding rebuild` のあとに、そうした chunk があるとき。
- 動作: エラーにせず、続けます。終了コードは変わりません。collection ごとに、collection 名と、そうした chunk の数を、警告として出します (`some chunks are longer than the embedding model's context length, …`)。その数は、`mdya status` の `truncated` でも見られます。
- 残るもの: そうした chunk も、インデックスに入ります。全文検索では、chunk のどの部分の文章でも見つかります。ベクトル検索では、コンテキスト長に収まらなかった部分の意味では、見つからないことがあります。
- 直し方: コンテキスト長がもっと長い埋め込みモデルに変えて (`mdya config set embedding.model`)、`mdya embedding rebuild` で作り直します。

## クエリが、埋め込みモデルのコンテキスト長を超える

- 起きるとき: ベクトル検索とハイブリッド検索で、クエリが、設定の埋め込みモデルのコンテキスト長を超えるとき。終了コード 1。MCP の `code` は `invalid_input`。
- 動作: 検索をせずに終わります。クエリの先頭だけで探すことも、ハイブリッド検索で全文検索の側だけで探すこともしません。
- 直し方: クエリを短くするか、全文検索 (`--mode fts`) で探します。コンテキスト長がもっと長い埋め込みモデルに変えることもできます。MCP のサーバーでは、設定を変えたあとに、サーバーを始め直します。

## インデックスや設定を読めない

- 起きるとき: インデックスや設定が壊れていて、読めないとき。たとえば、設定のファイルを直接書き換えて形が崩れた、ディスクの不具合でインデックスの一部が壊れた、などです。保守 ([コマンドの「インデックスを保守する」](commands.md#インデックスを保守する-mdya-optimize)) で、別のプロセスの検索が読んでいた状態が消えたときも、これに当たります (インデックスは壊れていません)。終了コード 1。MCP の `code` は `internal`。
- 動作: その操作をエラーで終えます。mdya は、自分で直したり、既定の値で代わりに動いたりしません。書き込み操作で、書き始めたあとに気づいたときは、それまでに書き終えたものが残ります。ただし、update、`mdya put`、`mdya embedding rebuild`、埋め込みモデルの設定を変える操作で、操作を終えたあとに、警告やメッセージのためにインデックスの状態を読めないときは、操作を終えたままにし、読めなかったことを警告 (設定を変える操作ではメッセージ) で伝えます。このときも、終了コードは 1 です。
- 直し方:
  - 保守の最中に検索したとき: もう一度検索します。
  - 設定のファイルを直接書き換えたとき: 書き換える前に戻すなどして、読める形に直します。
  - それで直らないとき、インデックスを読めないとき: 設定とインデックスのディレクトリ (既定は `~/.mdya`) を消して、collection を作り直し、取り込み直します。raw の collection の文書は、`mdya put` で渡し直します。設定も既定の値に戻るので、変えていた設定は、変え直します。

## インデックスや設定に書き込めない

- 起きるとき: 書き込み操作で、インデックスや設定に書き込めないとき。たとえば、ディスクがいっぱい、書き込む権限がない、などです。原因 (`Caused by:`) に、理由が出ます。終了コード 1。MCP のツールは読み取りだけなので、MCP では起きません。
- 動作: その場で止めます。それまでに書き終えたものは残ります。書き込み操作の最後の保守で書き込めなかったときは、操作を終えたままにし、保守に失敗したことを警告として出します。終了コードは、保守の失敗では変わりません。
- 直し方: 理由を取り除いて (ディスクを空ける、書き込む権限を直す、など)、もう一度同じ操作をします。

## ほかの書き込み操作が進んでいる

- 起きるとき: 同じ設定とインデックスのディレクトリで、ほかの書き込み操作 (`mdya update --all` など) が進んでいるときに、書き込み操作を始めたとき。終了コード 1。
- 動作: あとから始めた操作は、待たずに、何もせずに終わります。進んでいる操作は、そのまま続きます。
- 直し方: 進んでいる操作が終わってから、もう一度その操作をします。途中で止まった書き込み操作は、次の書き込み操作を止めません。

書き込み操作は、collection を作る・削除する・変える、update、`mdya put`、`mdya remove`、設定を変える、埋め込みベクトルを作り直す、保守する、です。検索などの読み取り操作は、書き込み操作の途中でも、待たずに結果を返します。そのとき、書き込み操作が書き終えた文書は新しい内容で、まだの文書は前の内容で見えます。

## 中断

- 起きるとき: 操作の途中で、Ctrl+C などで止めたとき、PC の電源が落ちたとき。終了コードは、OS の決まりのとおりです。
- 残るもの: 止めたところまでに書き終えた文書は、書き終えたあとの状態で残ります。途中だった文書は、前の状態か、書き終えたあとの状態です。埋め込みベクトルの作り直しを止めたときは、作りかけの埋め込みベクトルを、検索に使いません。
- 直し方: もう一度、同じ操作をします。`--force` を付けない update では、変わっていない文書を飛ばすので、止めたところの続きから進みます。`mdya embedding rebuild` では、もう一度作り直すと、作り直しを終えます。

## メモリの上限を超えた

- 起きるとき: mdya のメモリ使用量が、メモリの上限 (`resources.memory_limit_mb`) を超えたとき。終了コード 137。MCP のサーバーも止まります。
- 残るもの: [中断](#中断) と同じです。
- 直し方: メモリの上限を上げるか、並列の度合いを下げて、もう一度同じ操作をします ([設定の「メモリと並列の度合い」](configuration.md#メモリと並列の度合い))。

```
Error: mdya's memory usage went over the limit of 50 MB

How to fix:
    Raise the memory limit (mdya config set resources.memory_limit_mb) or lower the parallelism (mdya config set resources.embedding_parallelism), then run the operation again.
```

## 待ち受けるアドレスを使えない

- 起きるとき: `mdya mcp --http` で、待ち受けるアドレスを使えないとき。たとえば、ほかのプロセスがそのアドレスを使っている、PC にないアドレスを指定した、などです。終了コード 1。アドレスの形が正しくないときは、引数の使い方のエラー (終了コード 2) です。
- 直し方: `--address` で別のアドレスか番号を指定するか、そのアドレスを使っているプロセスを止めます。

```
Error: cannot listen on 127.0.0.1:18765

Caused by:
    0: Address already in use (os error 48)

How to fix:
    Give a different address or port with --address, or, if another process is using the address, stop it.
```

## MCP のクライアントとのやりとりを始められない

- 起きるとき: `mdya mcp` (stdio) で、MCP のクライアントとのやりとりを始められないとき。たとえば、やりとりの最初に要求でないものが来た、やりとりが始まる前に切れた、などです。終了コード 1。
- 直し方: MCP のクライアントの設定を確かめ、MCP のクライアントから MCP のサーバーを始め直します。

## 設定とインデックスのディレクトリを決められない

- 起きるとき: 設定とインデックスのディレクトリを決められないとき。たとえば、既定の値か `~` で始まる値で、ホームディレクトリがわからないとき、相対パスの値で、作業ディレクトリを読めないときです。終了コード 1。
- 直し方: `--data-dir` で、絶対パスを渡します。作業ディレクトリを読めないときは、読める作業ディレクトリに移ってやり直してもかまいません。

## この mdya が知らない collection タイプの collection がある

- 起きるとき: 新しいバージョンの mdya で作った collection タイプの collection を、この mdya で扱うとき。collection の場所を変える、update、`mdya put`、`mdya remove` で起きます。`mdya update --all` では、そうした collection が 1 つでもあれば起き、ほかの collection も update しません。終了コード 1。
- 直し方: その collection タイプを扱えるバージョンの mdya を使うか、`mdya collection remove` で、その collection を削除します。

## 想定外の失敗

- 起きるとき: mdya の不具合で、処理を続けられなくなったとき。CLI の終了コードは 101 です。MCP のサーバーでは、1 つの呼び出しの中で起きたときは、その呼び出しにだけエラーを返し (`code` は `internal`)、サーバーは動き続けます。
- 直し方: 不具合として、[issue](https://github.com/yoshihirosuzuki/mdya/issues) で報告してください。流したコマンド、`mdya --version` の出力、mdya が stderr に出したものを添えてください。

## 引数の使い方のエラー

- 起きるとき: コマンドの名前やオプションが正しくないとき、要る引数がないとき、全体のオプション (`--data-dir` など) をコマンドの名前のあとに書いたとき。終了コード 2。
- 直し方: `mdya --help` か、そのコマンドの `--help` で、書き方を確かめます。
