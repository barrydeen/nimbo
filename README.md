# Nimbo

[![CI](https://github.com/barrydeen/nimbo/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/barrydeen/nimbo/actions/workflows/ci.yml)
[![Android](https://github.com/barrydeen/nimbo/actions/workflows/android.yml/badge.svg?branch=main)](https://github.com/barrydeen/nimbo/actions/workflows/android.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Self-hosted, MIT-licensed markdown knowledge base and file drive with first-class
search and a built-in agent. Documents are plain `.md` files on disk, so your
vault is yours and works with any editor. Uploaded files live in a private,
content-addressed store and are searchable by name, extracted text, metadata and
automatic image descriptions.

> Status: early but usable. Auth, first-run setup, document CRUD, wikilinks,
> full-text search, the files module (folders, uploads, previews, metadata and
> vision extraction, public share links), device tokens for native clients, LLM
> provider settings and an agent layer (vault-grounded chat plus inline writing
> actions) are implemented. A companion Android backup app lives in `android/`.

## What works today

- First-run setup wizard that creates the administrator account
- Argon2id passwords, HttpOnly `SameSite=Strict` sessions, login rate limiting
- Document module: create/read/update/delete `.md` files in a vault directory
- `[[wikilinks]]` with backlinks, outgoing-link resolution and missing-link flags
- Optimistic concurrency: saves are rejected if the file changed underneath you
- Full-text search over the vault with ranked snippets (SQLite FTS5/BM25)
- Obsidian-style editor: CodeMirror source view, side-by-side live preview, markdown
  formatting toolbar, wikilink autocomplete
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
- Single-container Docker deployment with a persistent `/data` volume
- Companion Android backup app that mirrors phone folders to the drive over a
  device token (see [`android/README.md`](android/README.md))

## Quick start (Docker)

```bash
git clone https://github.com/barrydeen/nimbo.git
cd nimbo/docker
cp ../.env.example .env   # optional
docker compose up --build
```

Open <http://localhost:8080>. The first visit sends you to the setup wizard.
Create the admin account, then start writing.

The optional `.env` in `docker/` changes the session lifetime, upload limit or
secure-cookie setting. It is git-ignored; never commit it.

Data lives in the `nimbo_data` volume:

```
/data/nimbo.db      SQLite database (users, sessions, indexes, file tree, shares)
/data/secret.key    instance encryption secret (keep it with your backups)
/data/vault/        your markdown files
/data/files/blobs/  uploaded file content, addressed by SHA-256
/data/files/tmp/    transient upload and zip staging files
```

Back up `/data` as a whole. Markdown documents can be reindexed from `/data/vault`
on startup, but uploaded blobs and share links only exist in `/data/files` and
`nimbo.db`; `secret.key` must be preserved to decrypt provider API keys.

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

Then open the Vite URL (typically <http://localhost:5173>).

## Android backup app

The `android/` directory contains a native Kotlin/Jetpack Compose companion app
that mirrors selected phone folders to the Nimbo drive. It is built separately
with Gradle; see [`android/README.md`](android/README.md) for setup, signing and
the device-token flow.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `NIMBO_BIND` | `0.0.0.0:8080` | Listen address |
| `NIMBO_DATA_DIR` | `./data` | Database, secret and vault location |
| `NIMBO_STATIC_DIR` | unset | Serve the built frontend from this directory |
| `NIMBO_SECURE_COOKIES` | `false` | Mark session cookies `Secure` (use behind HTTPS) |
| `NIMBO_SESSION_TTL_DAYS` | `30` | Session lifetime in days |
| `NIMBO_MAX_UPLOAD_MB` | `200` | Maximum size of a single uploaded file |

## Security notes

- Put Nimbo behind a TLS-terminating reverse proxy and set
  `NIMBO_SECURE_COOKIES=true`.
- Cross-origin mutating requests are rejected by comparing `Origin` to `Host`.
- Path traversal is blocked: every document path is validated and resolved under
  the vault root, and only `.md` files are addressable.
- Document saves are capped at 8 MiB; uploaded files are capped by
  `NIMBO_MAX_UPLOAD_MB` (200 MiB by default).
- Uploaded blobs are stored by content hash, never by user-supplied path. Names
  and folder moves are validated, and public share traversal is checked against
  the shared subtree.
- Share tokens are 256-bit random values. Extracted text, metadata, EXIF/GPS and
  AI descriptions are never exposed on public share pages; only the owner sees
  them in the drive UI.
- Device tokens are 256-bit random `nmb_…` values. Only their SHA-256 hashes are
  stored, they can be revoked at any time, and they grant the same access as the
  user who created them. Treat them like passwords.
- Inline previews are limited to safe raster images, PDF, audio, video and plain
  text formats. HTML, SVG, XML and unknown types are forced to download.
- When image descriptions are enabled, image bytes are sent to the configured
  LLM provider. Disable automatic descriptions in Settings for fully local-only
  storage.

## API

All routes are JSON. Mutations require the session cookie.

```
POST /api/setup                 create admin (first run only)
POST /api/auth/login            sign in
POST /api/auth/logout           sign out
GET  /api/bootstrap             setup state + current user
GET  /api/instance              instance name and setup flag
GET  /api/tokens                list device tokens (hashes never returned)
POST /api/tokens                create a device token (shown once)
DELETE /api/tokens/{id}         revoke a device token
GET  /api/docs/tree             list indexed documents
GET  /api/docs/content?path=    read a document
PUT  /api/docs/content          create/update (base_hash for concurrency)
DELETE /api/docs/content?path=  delete a document
POST /api/docs/rename           move a document
GET  /api/docs/backlinks?path=  backlinks and outgoing links
GET  /api/search?q=             full-text search
GET  /api/settings/llm          provider settings (masked)
PUT  /api/settings/llm          save provider (admin only)
POST /api/settings/llm/test     connectivity check (admin only)
POST /api/agent/chat            streaming vault-grounded chat (SSE)
POST /api/agent/action          inline writing actions

GET  /api/files?folder_id=      list a folder (defaults to root)
POST /api/files/folders         create a folder
POST /api/files/upload?folder_id= multipart upload (one or more `file` fields)
GET  /api/files/recent          recently updated files
GET  /api/files/search?q=       search names, text, metadata and descriptions
GET  /api/files/stats           storage and share counters
GET  /api/files/{id}            file details, metadata, vision state and shares
PATCH /api/files/{id}           rename or move
DELETE /api/files/{id}          delete a file or folder subtree
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
GET  /api/public/shares/{token}/download.zip public folder zip
GET  /healthz                   health check
```

## Roadmap

1. File watcher so external edits are reindexed without a restart.
2. Semantic search: chunking, embeddings, hybrid BM25 + vector ranking.
3. Agent tool calls (search/read/write documents and files) with
   confirm-before-apply.
4. Resumable/chunked uploads and background zip jobs for very large folders.
5. Hardening: backups/restore tooling, upgrade path, release images.

## Documentation

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
