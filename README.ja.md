# mdya

*[English](README.md) / 日本語*

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#ライセンス)
[![Latest release](https://img.shields.io/github/v/release/yoshihirosuzuki/mdya)](https://github.com/yoshihirosuzuki/mdya/releases/latest)
[![CI](https://github.com/yoshihirosuzuki/mdya/actions/workflows/ci.yml/badge.svg)](https://github.com/yoshihirosuzuki/mdya/actions/workflows/ci.yml)

自分の PC にある Markdown と PDF の文書を、コマンドラインから、または AI アシスタントから MCP で検索するためのツールです。

## 概要

- 全文検索、ベクトル検索 (意味の近さで探す)、その 2 つを合わせたハイブリッド検索ができます。
- 検索する文書は、ディレクトリ、アーカイブ (zip、tar、gzip で圧縮した tar)、コマンドで渡した文書の内容から取り込みます。
- 使う入口は 2 つです。CLI のコマンドと、AI アシスタント (Claude Code など) から使う MCP のサーバーです。
- クラウドの LLM の API を使いません。埋め込みベクトル (ベクトル検索に使う、文章の意味を表す数値の列) は、PC の上で作ります。埋め込みベクトルを作ることを Ollama に任せる設定もあります。
- 1 つの実行ファイルで動きます。

mdya は、検索の結果を返すところまでを受け持ちます。クエリの書き換えや、結果の並べ替えのモデルは持ちません。

### 文書の言語

既定の設定は、日本語の文書向けです。既定の埋め込みモデル `cl-nagoya/ruri-v3-30m` は日本語のモデルで、全文検索は、日本語の辞書 (IPADIC) で語を切ります。

日本語のほかの言語の文書に使えそうな埋め込みモデルとして、`google/embeddinggemma-300m`、`sentence-transformers/all-MiniLM-L6-v2` (英語) と、Ollama のモデルを選べます。ただし、日本語のほかの言語の文書で、検索の精度は確かめていません ([説明書の「文書の言語」](docs/manual/ja/configuration.md#文書の言語))。

### 前のバージョンを使っていた人へ

mdya は作り直しました。作り直した mdya は、v0.4.3 以前の mdya と互換性がありません。

## 動く OS

macOS、Linux、Windows で動きます。リリースのページのビルド済みのバイナリは、次のものがあります。

- macOS (arm64、x86_64)
- Linux (x86_64、arm64)
- Windows (x86_64)

## インストール

```sh
# Linux と macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/yoshihirosuzuki/mdya/releases/latest/download/mdya-installer.sh | sh
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://github.com/yoshihirosuzuki/mdya/releases/latest/download/mdya-installer.ps1 | iex"
```

または、[リリースのページ](https://github.com/yoshihirosuzuki/mdya/releases) からビルド済みのバイナリを取得し、`mdya` を `PATH` の通った場所に置いてください。

[crates.io](https://crates.io/crates/mdya) からインストールすることもできます。ソースからビルドするので、下の「ソースからビルドする」の前提が要ります。

```sh
cargo install mdya
```

### ソースからビルドする

前提:

- Rust 1.95 以降 (リポジトリの `rust-toolchain.toml` が版を決めます)
- `protoc` (Protocol Buffers のコンパイラ。依存のクレートのビルドで使います)。macOS は `brew install protobuf`、Linux は `apt install -y protobuf-compiler`、Windows は `choco install protoc`
- ネットワーク。ビルドの途中で、依存のクレートと、全文検索の日本語の辞書 (`https://Lindera.dev/` から) を取得します。ネットワークにつながらない状態でビルドするときは、下の「ネットワークにつながらない状態でビルドする」を見てください。

```sh
git clone https://github.com/yoshihirosuzuki/mdya.git
cd mdya
cargo build --release        # ./target/release/mdya ができる
# または、PATH に置く:
cargo install --path .       # ~/.cargo/bin に mdya をインストールする
```

#### ネットワークにつながらない状態でビルドする

ビルドの途中で取得するもの (依存のクレートと、全文検索の辞書) を、ネットワークにつながっているうちに、ビルドする PC に取得しておきます。Rust と `protoc` も、先にインストールしておきます。

1. リポジトリのディレクトリで、依存のクレートを取得します。`cargo fetch` のあとは、`Cargo.lock` が変わらない限り、Cargo をネットワークなしで動かせます。

   ```sh
   cargo fetch --locked
   ```

2. 辞書の tarball を取得し、辞書を置く場所 (ここでは `$HOME/lindera`) の下の `3.0.7/` に置きます。`3.0.7` は、mdya が使う `lindera-ipadic` のクレートのバージョンで、`Cargo.lock` で確かめられます。

   ```sh
   mkdir -p "$HOME/lindera/3.0.7"
   curl -sSfL -o "$HOME/lindera/3.0.7/mecab-ipadic-2.7.0-20250920.tar.gz" https://Lindera.dev/mecab-ipadic-2.7.0-20250920.tar.gz
   ```

3. ネットワークにつながらない状態で、環境変数 `LINDERA_DICTIONARIES_PATH` に辞書を置く場所を入れ、`--frozen` を付けてビルドします。`--frozen` を付けると、Cargo はネットワークを使わず、`Cargo.lock` を変えようとしたときはエラーで止まります。

   ```sh
   LINDERA_DICTIONARIES_PATH="$HOME/lindera" cargo build --release --frozen
   ```

## はじめに

ディレクトリの Markdown を取り込んで、検索するまでの流れです。

```sh
mdya collection add notes ~/notes   # ~/notes を、collection "notes" にする
mdya update notes                   # ~/notes の .md、.markdown、.pdf を取り込む
mdya search "リリースの手順"           # 検索する
mdya get notes release.md           # 見つかった文書を読む
```

初めて取り込むとき (と、初めてベクトル検索かハイブリッド検索をするとき) に、埋め込みモデル (`cl-nagoya/ruri-v3-30m`、約 150 MB) を Hugging Face から取得し、`~/.mdya-models` に置きます。2 回目からは、ここから読みます。

検索の結果は、次のように出ます。

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

各ヒットは、collection 名と key、スコアと chunk の番号、スニペット (見つかったところの前後の文字) です。

ファイルを変えたら、`mdya update notes` を流すと、変わった文書だけを取り込み直します。すべての collection を取り込み直すときは、`mdya update --all` です。

ほかに、次のことができます。

- アーカイブ (`.zip`、`.tar`、`.tar.gz`、`.tgz`) を collection にする: `mdya collection add <名前> <アーカイブ> --type archive`
- コマンドで文書の内容を渡す: `mdya collection add <名前> --type raw` で collection を作り、`mdya put` で渡す
- 検索方式を選ぶ: `mdya search --mode fts` (全文検索)、`--mode vector` (ベクトル検索)、`--mode hybrid` (既定)
- 結果を JSON で受け取る: `--json`

## AI アシスタントから使う (MCP)

`mdya mcp` は MCP のサーバーとして動きます。Claude Code には、次のとおり登録します。

```sh
claude mcp add --transport stdio mdya -- mdya mcp
```

AI アシスタントは、MCP のツールで、collection の一覧を見る (`list_collections`)、検索する (`search`)、文書を取得する (`get_document`) ことができます。MCP のツールは読み取りだけです。collection を作ることと、文書を取り込むことは、上の「はじめに」のとおり、CLI で行ってください。

## 詳しい使い方

[説明書](docs/manual/ja/README.md) にあります。

- [collection と collection タイプ](docs/manual/ja/collections.md)
- [コマンド](docs/manual/ja/commands.md)
- [設定](docs/manual/ja/configuration.md)
- [MCP のサーバー](docs/manual/ja/mcp.md)
- [エラーと直し方](docs/manual/ja/errors.md)

## 開発

```sh
just check    # 書式の確かめ、clippy (警告はエラー)、cargo test (ワークスペース全体)、cargo audit
just          # レシピの一覧
```

`just check` には、「ソースからビルドする」の前提と `just` に加えて、`cargo-audit` (`cargo install cargo-audit --locked`) とネットワークが要ります (advisory のデータベースを毎回取得します)。ネットワークにつながらないときは、代わりに `cargo audit --no-fetch --stale` を流してください。

## コントリビュート

できる範囲で対応します (メンテナーは 1 人で、対応の期限は約束しません)。issue や PR を開く前に、次を読んでください。

- [CONTRIBUTING.md](CONTRIBUTING.md): issue と PR の流れ、ビルドとテストの手順。
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md): Contributor Covenant。
- [SECURITY.md](SECURITY.md): セキュリティの脆弱性を、公開せずに報告する方法。
- [SUPPORT.md](SUPPORT.md): 質問の窓口。

## ライセンス

MIT OR Apache-2.0 のデュアルライセンスです。[`LICENSE-MIT`](LICENSE-MIT) と [`LICENSE-APACHE`](LICENSE-APACHE) を見てください。
