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

- Authentication, session handling, device tokens and token-scope bypasses
- First-run setup claim or `NIMBO_SETUP_TOKEN` bypass
- Path traversal or access outside the vault/file store
- Public share link bypasses, including expired or revoked links
- Exposure of extracted text, metadata, EXIF/GPS or AI descriptions on public
  share pages
- Stored or reflected XSS in the web UI or public share pages
- Content-type confusion, unsafe inline rendering or response-splitting issues
- Denial of service caused by unbounded uploads, archive extraction or parser
  inputs
- Leakage of provider API keys, device tokens or the instance secret
- Deployment defaults that silently disable TLS, secure cookies or backup
  integrity

Out of scope:

- Vulnerabilities that require an already-compromised host or administrator
  account
- Missing security headers on a deliberately plain-HTTP development setup
- Reports from automated scanners without a demonstrated, reproducible impact
- Social engineering and physical access

## Deployment hardening

Nimbo is designed to be self-hosted. The repository's default Compose file
publishes only on `127.0.0.1`; operators are responsible for the network, TLS,
host and backups around it.

### First-run and network exposure

- Keep the loopback default until setup is complete. A fresh instance is
  claimable by the first client that reaches `POST /api/setup`.
- Set `NIMBO_SETUP_TOKEN` to a random one-time value before any remote setup.
  Generate it with `openssl rand -hex 32`; the setup request must include it.
- Put TLS live before remote credentials, session cookies or device tokens
  cross the network. Set `NIMBO_SECURE_COOKIES=true` only when every browser
  request uses HTTPS; the flag does not provide TLS.
- Do not publish the backend port directly. Use the supplied production Compose
  example (TLS proxy, no Nimbo port) or an equivalent proxy, and block bypass at
  the host firewall.
- The supplied proxy runs as unprivileged UID/GID 101 with a read-only root.
  The certificate directory and files must be owned by/readable by UID 101
  (owner-only `600` is fine when the files are owned by 101); root-owned
  mode-600 certificates are not readable. A custom proxy image/UID or Docker
  secret must use matching `uid`/`gid`/`mode` values.
- The proxy must preserve the browser `Host` header (`$http_host` in nginx).
  Nimbo rejects mutating requests whose `Origin` host differs from `Host`.
- Raise the proxy request-body limit to at least
  `NIMBO_MAX_UPLOAD_MB + 1 MiB`, and disable response buffering for SSE.

### Data at rest and backups

- Only provider API keys are encrypted by Nimbo, using `secret.key`. Notes,
  database rows, sessions, uploaded blobs and backups are plaintext. Use
  host/filesystem encryption and encrypted backup storage when disk theft or
  backup exposure is in scope.
- Back up the whole data volume **while Nimbo is stopped**, including
  `nimbo.db-wal`, `nimbo.db-shm` and `secret.key`. Do not copy a running
  database or selected paths. Follow the offline backup, integrity check,
  restore drill and rollback procedure in
  [`docs/operations.md`](docs/operations.md).
- A backup that has not been restored is unverified. Run the documented restore
  drill after deployment and before schema-changing upgrades.
- Keep the prior versioned image/digest and a pre-upgrade archive until the new
  version has passed its checks. Never run an older image against a database
  migrated by a newer one.
- Do not expose the data directory, database, key or backups to the internet.

### Credentials and sharing

- Prefer a **backup-scoped** device token for the Android app. It is confined
  to the `Phone Backups` subtree and cannot use docs, agent, settings, shares or
  token administration. A full-scope token has the account's access.
- Use HTTPS for the Android app. Plain HTTP requires an explicit insecure-allow
  setting and exposes the device credential; the app's automatic constraint is
  unmetered, not a specific Wi-Fi network.
- Revoke device tokens when a device is lost or retired. The raw token is shown
  once; only its hash is stored.
- Public share links grant read access to the shared subtree. Set expiries,
  revoke links that are no longer needed, and remember that folder zips are
  capped per archive (10,000 entries / 4 GiB) but still consume server disk and
  CPU.
- Expired sessions are rejected. Choose an explicit audit/session retention
  window and run the maintenance procedure in `docs/operations.md`; rotate
  container logs as configured.
- Keep Docker, the proxy, Rust dependencies and base images up to date. Release
  images are published only from version tags with provenance; production
  deployments should pin an exact version or digest. CI actions are pinned to
  reviewed full commit SHAs; container base/proxy images default to verified
  multi-arch digests; and the Gradle wrapper validates a pinned distribution
  checksum.
- Image descriptions send image bytes to the configured LLM provider. Disable
  automatic descriptions in Settings if that is not acceptable for your data.

## Handling secrets in this repository

The repository must never contain real credentials, API keys, tokens, keystores
or instance data. `.gitignore` excludes `.env`, runtime data directories,
databases, `secret.key`, Gradle keystores and local Android configuration;
`.dockerignore` also excludes review/test artifacts and secret material.

Release workflows use repository secrets only on trusted tag/manual runs:
`GITHUB_TOKEN` for GHCR publication and, when configured,
`ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and
`ANDROID_KEY_PASSWORD` for signed Android release builds. If the Android signing
secrets are absent, the release workflow builds no release APK rather than
falling back to a debug key. Pull-request workflows never receive these
secrets.

If you believe a secret has been committed, report it privately as above rather
than opening an issue. Assume the value is compromised and rotate it.
