# Contributing to mdya

Thanks for your interest in mdya. This document covers how to report issues and propose changes.

## Reporting issues

- **Bugs**: open a new issue using the **Bug report** template. The form prompts for the minimal repro context (mdya version, OS, what you tried, expected vs actual).
- **Feature requests**: open a new issue using the **Feature request** template. Read the README first — mdya is intentionally scoped to **local-first search** (no cloud LLM API, single binary distribution). Proposals that take mdya outside that scope will be declined.
- **Security vulnerabilities**: do not open a public issue. See [SECURITY.md](SECURITY.md).

## Submitting a pull request

1. Open or reference an issue describing the change.
2. Branch from `main`, make your change, and run `just check` (fmt + clippy + tests + advisory scan) before pushing. See [README.md](README.md#development) for what `just check` needs installed.
3. Open a pull request. The PR template prompts for a summary and a test plan.

Build and test setup is documented in [README.md](README.md#development).

## License

mdya is dual-licensed under the [MIT](LICENSE-MIT) and [Apache-2.0](LICENSE-APACHE) licenses. By submitting a contribution, you agree that your contribution is licensed under both, following the standard Apache-2.0 §5 "inbound = outbound" model.
