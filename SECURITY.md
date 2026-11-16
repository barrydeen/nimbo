# Security policy

## Supported versions

Nimbo is under active development. Security fixes are made on the latest release
and on the default branch (`main`). Older versions are not supported.

| Version | Supported |
| --- | --- |
| 0.1.x / `main` | Yes |
| Older | No |

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately through GitHub's
[private vulnerability reporting](https://github.com/barrydeen/nimbo/security/advisories/new)
form. If that is unavailable, contact the maintainer
[@barrydeen](https://github.com/barrydeen) privately on GitHub.

Please include:

- A description of the issue and the affected component
- Steps to reproduce, including a minimal request or file where possible
- The version or commit you tested
- The impact you believe is possible
- Any suggested fix or mitigation

You can expect an acknowledgement within a few days. Please give the maintainer
a reasonable window to investigate and ship a fix before public disclosure.

## Scope

In scope:

- Authentication and session handling
- Path traversal or access outside the vault/file store
- Public share link bypasses, including expired or revoked links
- Exposure of extracted text, metadata, EXIF/GPS or AI descriptions on public
  share pages
- Stored or reflected XSS in the web UI or public share pages
- Content-type confusion, unsafe inline rendering or response-splitting issues
- Denial of service caused by unbounded uploads, archive extraction or parser
  inputs
- Leakage of provider API keys or the instance secret

Out of scope:

- Vulnerabilities that require an already-compromised host or administrator
  account
- Missing security headers on a deliberately plain-HTTP development setup
- Reports from automated scanners without a demonstrated, reproducible impact
- Social engineering and physical access

## Deployment hardening

Nimbo is designed to be self-hosted. Operators are responsible for the network
and host around it:

- Terminate TLS in front of Nimbo and set `NIMBO_SECURE_COOKIES=true`.
- Do not expose the database, data directory or backups to the internet.
- Back up `/data` as a whole. `secret.key` is required to decrypt stored
  provider API keys; losing it invalidates them.
- Keep Docker, Rust dependencies and the base images up to date.
- Image descriptions send image bytes to the configured LLM provider. Disable
  automatic descriptions in Settings if that is not acceptable for your data.

## Handling secrets in this repository

The repository must never contain real credentials, API keys, tokens, keystores
or instance data. `.gitignore` excludes `.env`, runtime data directories,
databases, `secret.key`, Gradle keystores and local Android configuration.

If you believe a secret has been committed, report it privately as above rather
than opening an issue. Assume the value is compromised and rotate it.
