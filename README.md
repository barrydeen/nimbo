# Nimbo

[![CI](https://github.com/barrydeen/nimbo/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/barrydeen/nimbo/actions/workflows/ci.yml)
[![Android](https://github.com/barrydeen/nimbo/actions/workflows/android.yml/badge.svg?branch=main)](https://github.com/barrydeen/nimbo/actions/workflows/android.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Self-hosted, MIT-licensed markdown knowledge base and file drive with first-class
search and a built-in agent. Documents are plain `.md` files on disk, so your
vault is yours and works with any editor. Uploaded files live in a private,
content-addressed store and are searchable by name, extracted text, metadata and
automatic image descriptions.

> Status: early but usable. Auth, first-run setup with an optional bootstrap
> token, document CRUD, wikilinks, sanitized previews, full-text search, the
> files module (folders, uploads, previews, metadata and vision extraction,
> public share links), device tokens with optional backup scope, LLM provider
> settings and an agent layer (vault-grounded chat plus inline writing actions)
> are implemented. A companion Android backup app lives in `android/`.

Nimbo encrypts provider API keys at rest. Notes, database rows, uploaded blobs
and backups are **not** application-encrypted; see [data at rest](#security-notes)
and the [operations runbook](docs/operations.md).

## What works today

- First-run setup wizard that creates the administrator account, with an
  optional one-time `NIMBO_SETUP_TOKEN` bootstrap secret
- Argon2id passwords, HttpOnly `SameSite=Strict` sessions, login rate limiting
- Document module: create/read/update/delete `.md` files in a vault directory
- `[[wikilinks]]` with backlinks, outgoing-link resolution and missing-link flags
- Optimistic concurrency: saves are rejected if the file changed underneath you
- Full-text search over the vault with ranked snippets (SQLite FTS5/BM25)
- Obsidian-style editor: CodeMirror source view, side-by-side live preview,
  markdown formatting toolbar, wikilink autocomplete, sanitized HTML and URLs
- Agent: connect any OpenAI-compatible provider (OpenAI, Ollama, LM Studio, vLLM,
  OpenRouter); API keys encrypted at rest
- Vault-grounded streaming chat that retrieves relevant documents before answering
- Inline editor actions: Tab-accepted next-line suggestions (toggleable), grammar,
  rewrite, link suggestions and tags
- Files drive: folders, drag-and-drop uploads, streaming downloads with byte
  ranges, grid/list browsing, recent files and search
- Metadata and content extraction for images (dimensions, EXIF, thumbnails), PDFs
  (document info + text), Office/ODF/EPUB documents, audio tags, and text/code
- Automatic image descriptions (description, visible text, tags) through any
  OpenAI-compatible vision model
- Revocable, optionally expiring public share links for files and folders, with
  folder zip downloads
- Device tokens with full or backup scope; the Android app uses a token confined
  to its `Phone Backups` subtree
- Single-container Docker deployment with a persistent `/data` volume, a
  loopback-only default, resource limits, health checks and log rotation
- Versioned container images published to GHCR on release tags, plus a
  Docker build/persistence smoke test, dependency audits and an integrated
  release-binary security regression suite in CI
- Digest-pinned container bases and full-commit-SHA GitHub Actions references
- CI Android release workflow that produces no release APK when signing secrets
  are absent, instead of falling back to a debug key
- Companion Android backup app that mirrors phone folders to the drive over a
  device token (see [`android/README.md`](android/README.md))

## Quick start (Docker)

Requirements: Docker Engine with Compose v2, and `openssl` for the optional
bootstrap token.

```bash
git clone https://github.com/barrydeen/nimbo.git
cd nimbo/docker
cp ../.env.example .env
token="$(openssl rand -hex 32)"
printf 'NIMBO_SETUP_TOKEN=%s\n' "$token" >> .env
printf 'Setup token: %s\n' "$token"
docker compose up --build -d
docker compose logs -f nimbo
```

Open <http://127.0.0.1:8080>. The setup page asks for the bootstrap token from
`docker/.env`; create the admin account, then start writing.

The default Compose publication is **loopback only** (`127.0.0.1:8080`). That
means another machine cannot reach first-run setup. To change the local port:

```bash
NIMBO_PUBLISH_PORT=8081 docker compose up -d
```

Do not set `NIMBO_PUBLISH_HOST=0.0.0.0` on a fresh instance. If you need remote
access, complete setup through an SSH tunnel or put TLS in front first; see
[First run and bootstrap](docs/operations.md#first-run-and-bootstrap) and
[TLS and reverse proxy](docs/operations.md#tls-and-reverse-proxy).

The optional `.env` in `docker/` changes the publication, session lifetime,
upload limit, setup token, resource limits or secure-cookie setting. It is
git-ignored; never commit it. If `NIMBO_SETUP_TOKEN` is omitted, setup is
unauthenticated but still reachable only on the loopback default.

Stop the stack with:

```bash
docker compose down
```

### Production-shaped deployment

[`docker/docker-compose.production.yml`](docker/docker-compose.production.yml)
provides an nginx TLS proxy that runs unprivileged, publishes no Nimbo port,
sets secure cookies, and requires an explicit image version, public domain and
setup token. TLS files must be owned by/readable by proxy UID/GID 101; see the
[operations runbook](docs/operations.md) for certificates, header/origin
behavior, upload limits, SSE buffering, backups and rollback.

## Data layout

Data lives in the named volume `nimbo_nimbo_data`:

```
/data/nimbo.db          SQLite database (users, sessions, indexes, file tree, shares)
/data/nimbo.db-wal      write-ahead log (may be present)
/data/nimbo.db-shm      shared-memory index (may be present)
/data/secret.key        instance key for encrypted provider credentials
/data/vault/            your plaintext markdown files
/data/files/blobs/      uploaded file content and thumbnails, addressed by SHA-256
/data/files/tmp/        transient upload and zip staging files
```

Markdown documents can be reindexed from `/data/vault` on startup, but uploaded
blobs and share links only exist in `/data/files` and `nimbo.db`; `secret.key`
must be preserved to decrypt provider API keys.

### Backup in one sentence

Back up the **entire volume while Nimbo is stopped**, including
`nimbo.db-wal`, `nimbo.db-shm` and `secret.key`; do not copy a running database
or selected files. The full procedure, integrity checks, restore drill and
rollback steps are in [`docs/operations.md`](docs/operations.md#backup).

## Local development

Requirements: Rust stable, Node.js 22+ and npm. SQLite is embedded, so there is
no external database to run.

```bash
# terminal 1 - backend
cd backend
NIMBO_DATA_DIR=../data cargo run

# terminal 2 - frontend (proxies /api to :8080)
cd frontend
npm install
npm run dev
```

Then open the Vite URL (typically <http://localhost:5173>). Local development
uses HTTP; do not expose that listener beyond your machine.

## Android backup app

The `android/` directory contains a native Kotlin/Jetpack Compose companion app
that mirrors selected phone folders to the Nimbo drive. It is built separately
with Gradle; see [`android/README.md`](android/README.md) for setup, signing and
the device-token flow.

- Use **HTTPS**. Plain HTTP requires an explicit insecure-allow setting in the
  app, sends the device credential in cleartext, and is not recommended.
- Prefer a **backup-scoped** device token. It is bound to the `Phone Backups`
  subtree and cannot read documents, change settings, manage shares or create
  more tokens. A full-scope token has the same access as the account.
- The automatic work constraint is **unmetered**, not "a specific Wi-Fi
  network". Test & Save is a direct request and ignores that constraint.
- Android's Storage Access Framework restricts some directories; not every
  folder on the device can be selected.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `NIMBO_PUBLISH_HOST` | `127.0.0.1` | Host interface for the Compose-published port |
| `NIMBO_PUBLISH_PORT` | `8080` | Host port for the Compose-published port |
| `NIMBO_BIND` | `127.0.0.1:8080` | Listen address; Compose sets `0.0.0.0:8080` inside the container |
| `NIMBO_DATA_DIR` | `./data` (`/data` in the image) | Database, secret, vault and blobs |
| `NIMBO_STATIC_DIR` | unset (`/app/static` in the image) | Serve the built frontend |
| `NIMBO_SETUP_TOKEN` | unset | One-time bootstrap secret for `POST /api/setup` (16–512 bytes; omit when disabled) |
| `NIMBO_SECURE_COOKIES` | `false` | Mark session cookies `Secure`; requires HTTPS |
| `NIMBO_SESSION_TTL_DAYS` | `30` | Session lifetime in days (1–3650) |
| `NIMBO_MAX_UPLOAD_MB` | `200` | Per-file upload ceiling (1–10240 MiB) |
| `NIMBO_AUDIT_RETENTION_DAYS` | `90` | Retain audit rows for this many days (1–3650) |
| `NIMBO_CPUS` / `NIMBO_MEMORY` | `2.0` / `2g` | Container resource limits |
| `NIMBO_LOG` | `nimbo=info,tower_http=info` | Log filter |

The production example additionally uses `NIMBO_IMAGE`, `NIMBO_DOMAIN`,
`NIMBO_TLS_DIR`, `NIMBO_EDGE_BIND`, `NIMBO_HTTP_PORT`, `NIMBO_HTTPS_PORT` and
`NIMBO_PROXY_BODY_LIMIT`. The complete reference and effective request limits
are in [`docs/operations.md`](docs/operations.md#configuration-reference).

## Security notes

- Put Nimbo behind a TLS-terminating reverse proxy and set
  `NIMBO_SECURE_COOKIES=true`. The flag does not add TLS by itself. Preserve the
  browser `Host` header; cross-origin mutating requests are rejected by
  comparing `Origin` to `Host`.
- Keep the default loopback publication, or require `NIMBO_SETUP_TOKEN` and
  complete setup over HTTPS/tunnels. Never expose a fresh plain-HTTP instance
  to an untrusted network.
- Do not publish the backend port in production. The supplied proxy example has
  no direct Nimbo port; block container-port bypass at the host firewall.
- A note's decoded content is capped at 8 MiB and the save JSON body at 16 MiB.
  A single uploaded file is capped by `NIMBO_MAX_UPLOAD_MB` (200 MiB default),
  the whole multipart request is capped at about that value + 1 MiB, and at
  most 100 files are accepted per request. Batches can hit the aggregate cap
  even when every file is individually valid.
- Public folder archives are capped per archive at 10,000 entries and 4 GiB of
  source bytes. At most two archives build concurrently, with an 8 GiB aggregate
  staging cap; they use the same volume as the database, so concurrent public
  downloads still need disk and CPU headroom.
- Uploaded blobs are stored by content hash, never by user-supplied path. Names
  and folder moves are validated, and public share traversal is checked against
  the shared subtree.
- Share tokens are 256-bit random values. Extracted text, metadata, EXIF/GPS and
  AI descriptions are never exposed on public share pages; only the owner sees
  them in the drive UI.
- Device tokens are 256-bit random `nmb_…` values. Only their SHA-256 hashes are
  stored, they can be revoked at any time, and the raw value is shown once.
  Full-scope tokens grant the creator's access; prefer backup scope for the
  Android app. Treat both like passwords.
- Markdown preview and search snippets are rendered through a maintained
  sanitizer with an explicit URL policy; raw HTML and event handlers are not
  trusted. Inline previews are limited to safe raster images, PDF, audio, video
  and plain text formats.
- Only provider API keys are encrypted at rest, using `secret.key`. Notes,
  database rows, blobs and backups are plaintext. Use host/filesystem
  encryption and protected backup storage if disk theft or backup exposure is
  in scope.
- When image descriptions are enabled, image bytes are sent to the configured
  LLM provider. Disable automatic descriptions in Settings for fully local-only
  storage.

## API

All routes are JSON. Mutations require the session cookie or a device bearer
token, except setup/login and public share routes.

```
POST /api/setup                 create admin (first run only; setup_token when configured)
GET  /api/setup/status          setup state and setup_token_required
POST /api/auth/login            sign in
POST /api/auth/logout           sign out
GET  /api/auth/me               current session/device user
GET  /api/bootstrap             setup state, setup_token_required and current user
GET  /api/instance              instance name, setup flag and max_upload_bytes
GET  /api/tokens                list device tokens (hashes never returned)
POST /api/tokens                create a token (optional scope: full or backup)
DELETE /api/tokens/{id}         revoke a device token
GET  /api/docs/tree             list indexed documents
GET  /api/docs/content?path=    read a document
PUT  /api/docs/content          create/update (8 MiB content, 16 MiB JSON body)
DELETE /api/docs/content?path=  delete a document
POST /api/docs/rename           move a document
GET  /api/docs/backlinks?path=  backlinks and outgoing links
GET  /api/search?q=             full-text search
GET  /api/settings/llm          provider settings (masked)
PUT  /api/settings/llm          save provider (admin only)
POST /api/settings/llm/test     connectivity check (admin only)
POST /api/agent/chat            streaming vault-grounded chat (SSE)
POST /api/agent/action          inline writing actions

GET  /api/files?folder_id=      list a folder (root default; offset/limit, next_offset, sha256)
POST /api/files/folders         create a folder
POST /api/files/upload?folder_id= multipart upload (one or more `file` fields)
GET  /api/files/recent          recently updated files
GET  /api/files/search?q=       search names, text, metadata and descriptions
GET  /api/files/stats           storage and share counters
GET  /api/files/{id}            file details, metadata, vision state and shares
PATCH /api/files/{id}           rename or move
DELETE /api/files/{id}          delete a file or folder subtree
PUT  /api/files/{id}/content    replace file bytes in place (multipart `file`)
GET  /api/files/{id}/content    inline content (supports Range)
GET  /api/files/{id}/download   attachment download
GET  /api/files/{id}/thumbnail  generated image thumbnail
POST /api/files/{id}/vision     retry image description
POST /api/files/{id}/reindex    retry metadata/text extraction
GET  /api/shares                list share links
POST /api/shares                create a file or folder share
PATCH /api/shares/{id}          change or clear expiry
DELETE /api/shares/{id}         revoke a share
GET  /api/public/shares/{token} public share metadata
GET  /api/public/shares/{token}/nodes public folder browse
GET  /api/public/shares/{token}/files/{id}/download public download
GET  /api/public/shares/{token}/files/{id}/content public inline content
GET  /api/public/shares/{token}/files/{id}/thumbnail public thumbnail
GET  /api/public/shares/{token}/download.zip public folder zip
GET  /livez                      lightweight liveness check
GET  /healthz                    readiness check (database and data paths)
```

Backup-scoped tokens may use instance/auth-self/logout and file routes only;
document, agent, settings, share and token-administration routes return an
authorization error.

## Roadmap

1. File watcher so external edits are reindexed without a restart.
2. Semantic search: chunking, embeddings, hybrid BM25 + vector ranking.
3. Agent tool calls (search/read/write documents and files) with
   confirm-before-apply.
4. Resumable/chunked uploads and background zip jobs for very large folders.
5. Built-in backup scheduling and point-in-time recovery tooling.

## Documentation

- [`docs/operations.md`](docs/operations.md) - deployment, TLS/bootstrap
  sequence, offline backup/restore, restore drills, upgrades, rollback,
  retention, logs and FAQ
- [`docs/architecture.md`](docs/architecture.md) - components, data flow, threat
  model and known limitations
- [`CONTRIBUTING.md`](CONTRIBUTING.md) - development setup, checks and PR process
- [`SECURITY.md`](SECURITY.md) - reporting vulnerabilities and deployment
  hardening
- [`CHANGELOG.md`](CHANGELOG.md) - release history

## Contributing

Issues and pull requests are welcome. Please read
[`CONTRIBUTING.md`](CONTRIBUTING.md) and the
[Code of Conduct](CODE_OF_CONDUCT.md) first. For security problems, use the
private reporting process in [`SECURITY.md`](SECURITY.md) instead of a public
issue.

## License

MIT. See [LICENSE](LICENSE).
