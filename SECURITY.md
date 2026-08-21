# Security policy

## Supported versions

Only the latest minor release of mdya receives security updates. Older minor versions are not maintained.

| Version | Supported          |
| ------- | ------------------ |
| 0.4.x   | :white_check_mark: |
| < 0.4   | :x:                |

## Reporting a vulnerability

**Please do not file a public GitHub issue** for security vulnerabilities. Use one of the following private channels instead:

1. **Preferred — GitHub Private Vulnerability Reporting**: open a report at <https://github.com/yoshihirosuzuki/mdya/security/advisories/new>. The repository maintainer receives the report privately and can coordinate a fix and disclosure.
2. **Alternative — email**: send a description of the issue to `1631550+yoshihirosuzuki@users.noreply.github.com`. This is forwarded by GitHub to the repository maintainer.

When reporting, please include:

- A clear description of the vulnerability and its potential impact.
- Steps to reproduce, or a proof-of-concept.
- The mdya version (`mdya --version`) and the platform you observed it on.

Acknowledgement of reports usually happens within a few business days. There is no formal SLA — mdya is maintained by one person.

## Informational advisories

`cargo audit` runs daily on a schedule, and a failing run opens an issue labelled `security-advisory`. It also runs locally as part of `just check`. Upstream crates flagged as `unmaintained` (informational, not CVE) that mdya cannot fix directly are tracked in [`.cargo/audit.toml`](.cargo/audit.toml). Each ignored advisory is reviewed when an upstream release drops the crate. CVE / RUSTSEC security advisories are never suppressed.

## How fixes are shipped

A vulnerability in one of mdya's dependencies is fixed by moving that dependency and shipping the result in the next release, recorded under `Security` in [CHANGELOG.md](CHANGELOG.md). A separate GitHub Security Advisory is published only when mdya's own code is at fault, or when a dependency flaw is reachable from outside the machine in a default configuration — the MCP server binds to loopback unless you point `--addr` elsewhere.

How you installed mdya decides when a fix reaches you. Prebuilt binaries and the shell / PowerShell installers are built from the lockfile of the tag they were cut from, so they pick up a dependency fix only when a new version is released. `cargo install mdya` ignores the packaged lockfile by default and re-resolves dependencies, which can pull in a patched dependency without waiting for an mdya release; `cargo install --locked mdya` opts out and reproduces the exact tree the release was built with.
