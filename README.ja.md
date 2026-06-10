# mdya

*[English](README.md) / 日本語*

ローカルの Markdown コレクションのための、小さく速い検索プリミティブ。BM25 全文検索・on-device ベクトル検索・両者を束ねる hybrid 検索を、**CLI** と **MCP サーバ**の 2 つの入口から提供します。

## 概要

mdya は **検索プリミティブ**です。ローカルの Markdown を索引化し、検索結果を CLI と MCP サーバ越しに返す — それが仕事の全てです。

mdya は検索エージェントでも、LLM によるクエリ書き換えフロントでもありません。クエリ拡張・reranker・多段エージェント pipeline は持ちません。そうした処理が欲しいワークフローは、mdya を CLI / MCP 越しに呼ぶ別ツールを上に積む設計を想定しています。

ローカルファーストを徹底しています。クラウド LLM API への経路は無く、推論はすべて on-device で完結し、配布は単一バイナリです。

できること:

- **BM25 全文検索** — lindera/ipadic による日本語形態素解析に対応した全文検索。
- **ベクトル検索** — on-device embedding（default `cl-nagoya/ruri-v3-30m`, 256 次元）による cosine 類似検索。
- **hybrid 検索** — BM25 とベクトルの結果を Reciprocal Rank Fusion (RRF) で統合。
- **Markdown チャンキング** — 見出し・コードフェンス境界を意識した分割。
- **MCP サーバ** — `mdya mcp`（stdio / HTTP）で検索ツールを MCP クライアントへ公開。

`.pdf` 形式のファイルも `.md` と同じ取り込み経路を通ります。 取り込み時に plain text に変換した上で、 Markdown と共通のチャンク分割・埋め込みを行います。

## サポート platform

- macOS（arm64 / x86_64）
- Linux（amd64 / arm64）
- Windows（x86_64）

## インストール

サポートする全 platform 向けのプリビルドバイナリが各 GitHub Release に同梱されています。下記のインストーラスクリプトは**最新の release** からお使いの platform に合ったアーカイブを取得し、`mdya` バイナリを `$CARGO_HOME/bin`（default は `~/.cargo/bin`）に配置します。

Linux / macOS の場合:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/yoshihirosuzuki/mdya/releases/latest/download/mdya-installer.sh | sh
```

Windows (PowerShell) の場合:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/yoshihirosuzuki/mdya/releases/latest/download/mdya-installer.ps1 | iex"
```

特定バージョンに pin したり platform 別アーカイブ（`.tar.xz` / `.zip`）を直接取りたい場合は[リリースページ](https://github.com/yoshihirosuzuki/mdya/releases)を参照してください。

### ソースからビルド

前提:

- Rust 1.95 以降（リポジトリ root の `rust-toolchain.toml` が自動 pin）
- `just`（`cargo install just`）
- `protoc`（Protocol Buffers compiler、`lance` の build 依存）。macOS は `brew install protobuf`、Linux は `apt install -y protobuf-compiler`、Windows は `choco install protoc`

```sh
git clone https://github.com/yoshihirosuzuki/mdya.git
cd mdya
cargo build --release        # ./target/release/mdya を生成
# あるいは PATH に通す:
cargo install --path .       # mdya を ~/.cargo/bin へインストール
```

## はじめに

最短の流れは `init` → `collection add` → `update-all` → `search` です。

```sh
mdya init                       # ~/.mdya/ を作成 (config.yml + 索引などのデータディレクトリ)
mdya collection add ~/notes     # ディレクトリを collection として登録 (名前は basename = notes)
mdya update-all                 # 登録済み collection を走査し .md / .pdf を取り込んで索引を構築
mdya search fts "リリース手順"     # BM25 全文検索
mdya search vector "リリース手順"  # ベクトル検索
mdya search hybrid "リリース手順"  # hybrid (RRF)
```

> 初回の `mdya update-all`（およびベクトル / hybrid 検索）で、embedding model（`cl-nagoya/ruri-v3-30m`, 約 140MB）を `~/.mdya-models/` に自動ダウンロードします。以降はこのキャッシュを使います。

検索結果は人間向けフォーマットで標準出力に出力されます。

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

主なフラグ:

- `-c, --collections <名前...>` — 対象 collection を絞る（省略時は全 collection）
- `-n, --limit <N>` — 上位 N 件（default 20）
- `--format <形式>` — 出力形式を選ぶ（default `human`）。`json`（機械処理向けエンベロープ）/ `md`（Markdown、貼り付け・LLM 入力向け）/ `xml`（JSON と等価な構造、LLM 入力向け）も指定可能

## MCP サーバ連携

mdya は MCP（Model Context Protocol）サーバとして起動でき、Claude Code / Claude Desktop などのクライアントから検索 tool を呼べます。

```sh
mdya mcp        # stdio トランスポートで MCP サーバを起動
```

Claude Code への登録例:

```sh
claude mcp add mdya -- mdya mcp
```

## 詳しい使い方

設定ファイル・各サブコマンド・MCP 連携の詳細は[ユーザマニュアル](docs/manual/ja/README.md)を参照してください。

## 設定

設定は `~/.mdya/config.yml` の 1 ファイルにまとまっています（`mdya init` が雛形を生成）。最小例:

```yaml
collections:
  notes:
    path: ~/notes
embedding:
  model: cl-nagoya/ruri-v3-30m
runtime:
  memory_limit_mb: 8192     # 自プロセスの RSS 上限 (MB)。0 で無効
  embed_parallelism: 8      # update-all で並列に embedding するファイル数。0 で逐次
```

`mdya collection add <path>` が `collections` セクションを書き換えるので、通常は手で編集する必要はありません。設定ディレクトリは `--config-dir <path>` で、埋め込みモデルキャッシュは `--model-cache-dir <path>`（default は `$HOME/.mdya-models/`）でそれぞれ切り替えられます。

## 開発

```sh
just check    # fmt-check + clippy (-D warnings) + cargo test (全 workspace)
just smoke    # smoke テストのみ実行
just          # recipe 一覧を表示
```

## ライセンス

MIT OR Apache-2.0 のデュアルライセンス。[`LICENSE-MIT`](LICENSE-MIT) と [`LICENSE-APACHE`](LICENSE-APACHE) を参照してください。
