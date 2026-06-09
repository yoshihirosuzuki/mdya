# mdya ユーザマニュアル

mdya はローカルの Markdown コレクションを索引化し、検索結果を CLI と MCP サーバ越しに返すツールです。BM25 全文検索・on-device ベクトル検索・両者を束ねる hybrid 検索の 3 モードを提供します。

このマニュアルは mdya を**使う側**のためのリファレンスです。インストールと最短の使い方はリポジトリの [README](../../../README.ja.md) に書かれています。このマニュアルでは、設定ファイルとサブコマンドの詳細を扱います。

## 目次

- [設定](configuration.md) — `~/.mdya/config.yml` の各項目、ディレクトリレイアウト、設定ディレクトリの切り替え方。
- [コマンドリファレンス](commands.md) — 各サブコマンドの引数・オプション・出力。
- [MCP サーバ連携](mcp.md) — `mdya mcp` の使い方、提供されるツール、Claude Code 等への登録方法。

## このマニュアルが扱わない範囲

- インストール手順とクイックスタート → [README](../../../README.ja.md)
- ライセンス → [`LICENSE-MIT`](../../../LICENSE-MIT) / [`LICENSE-APACHE`](../../../LICENSE-APACHE)
