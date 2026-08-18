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
