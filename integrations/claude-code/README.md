# mdya for Claude Code

Query and search a local Markdown corpus indexed by [mdya](https://github.com/yoshihirosuzuki/mdya) from inside Claude Code.

This plugin adds two skills and bundles the mdya MCP server:

- `/mdya:query <question>` — answers your question grounded in the corpus, with numbered citations.
- `/mdya:search <query>` — returns raw hybrid-search hits, with no LLM synthesis.

## Prerequisites

- `mdya` installed and on your `PATH`.
- A Markdown corpus already indexed by mdya (the plugin uses mdya's default config directory).

## Install

```
/plugin marketplace add yoshihirosuzuki/mdya
/plugin install mdya@mdya-plugins
```

## Usage

```
/mdya:query What does the project say about retries?
/mdya:search retry backoff
```
