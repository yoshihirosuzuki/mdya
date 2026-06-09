# mdya user manual

mdya indexes local Markdown collections and returns search results over a CLI and an MCP server. It offers three search modes: BM25 full-text search, on-device vector search, and a hybrid that fuses both.

This manual is a reference for **people who use mdya**. Installation and the shortest path to a working setup are covered in the [README](../../../README.md). The pages here go deeper into the configuration file and the individual subcommands.

## Table of contents

- [Configuration](configuration.md) — every option in `~/.mdya/config.yml`, the directory layout, and how to relocate the configuration directory.
- [Command reference](commands.md) — arguments, options, and output for each subcommand.
- [MCP server](mcp.md) — how to run `mdya mcp`, which tools it exposes, and how to register it with Claude Code and similar clients.

## Not covered here

- Installation and quick start → [README](../../../README.md)
- License → [`LICENSE-MIT`](../../../LICENSE-MIT) / [`LICENSE-APACHE`](../../../LICENSE-APACHE)
