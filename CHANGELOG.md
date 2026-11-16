# Changelog

All notable changes to Nimbo are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `docs/operations.md`: offline whole-volume backup/restore, integrity and blob
  checks, restore drills, versioned upgrades, rollback, retention, log rotation
  and an operational FAQ
- `scripts/verify-operations.py`: deterministic checks for Compose defaults,
  documentation claims, CI job coverage and committed secrets/private files
- Docker build and persistence/health smoke test in CI, plus deterministic
  production-dependency audits
- Integrated release-binary security regression gate covering setup tokens,
  login bounding, snippet escaping, symlinks, serialized document/tree
  mutations, backup-token scope/replacement, ranges, framing, origin checks and
  public payload minimization
- Versioned GHCR image publication on `v*.*.*` tags with provenance, SBOM and
  OCI source/revision labels
- Android unit-test, lint, wrapper-validation and release-signing workflow
  coverage; release builds fail closed when signing secrets are absent
- Backup-scoped device tokens and the in-place file replacement endpoint

### Changed

- Compose now publishes only on `127.0.0.1` by default, supports a configurable
  host/port, and includes resource limits, read-only root filesystems, dropped
  capabilities, health checks, graceful stop time and rotated logs
- Production Compose example provides an nginx TLS proxy with no direct Nimbo
  port, preserved `Host`/`Origin` behavior, upload body limits and unbuffered
  SSE; the proxy runs as unprivileged UID/GID 101 with owned tmpfs paths
- Docker image metadata now records version, revision and source; runtime runs
  as fixed UID/GID 10001 with a read-only-friendly layout
- Quick start and configuration docs now describe effective document, upload
  and public-archive limits accurately

### Security

- First-run setup supports a mandatory `NIMBO_SETUP_TOKEN`; remote setup must
  happen after TLS is live
- Security documentation now states that only provider API keys are encrypted
  at rest; notes, database rows, blobs and backups are plaintext
- Token-scope, HTTPS and unmetered-network guidance corrected for the Android
  app
- Android release signing is fail-closed in CI; no debug-signed release artifact
  is produced when signing secrets are unavailable
- CI actions are pinned to reviewed full commit SHAs, and Docker base/proxy
  images default to verified multi-arch digests; the Gradle wrapper
  distribution checksum is pinned and validated
- SQLx was upgraded to 0.9 and its SQLite driver is locally patched to bundle
  `libsqlite3-sys` 0.39.1/SQLite 3.54.1, resolving RUSTSEC-2026-0459; the Rust
  audit now has no vulnerabilities (two compile-only unmaintained-crate warnings
  remain with no fixed upgrade)
- `.dockerignore` excludes review/test artifacts, runtime data and credential
  material from image builds

### Fixed

- Corrected the extraction time-budget and encryption-at-rest claims in the
  architecture documentation
- Clarified the 8 MiB document/16 MiB JSON body limits and per-file versus
  aggregate multipart upload limits

### Planned

- File watcher so external vault edits are reindexed without a restart
- Semantic search with chunking, embeddings and hybrid BM25 + vector ranking
- Agent tool calls with confirm-before-apply writes
- Resumable uploads and background folder-zip jobs
- Built-in backup scheduling and point-in-time recovery tooling

## [0.1.0] - 2026-11-15

First public release.

### Added

- First-run setup wizard that creates the administrator account
- Argon2id password hashing, HttpOnly `SameSite=Strict` sessions and login rate
  limiting
- Markdown vault with create/read/update/delete, atomic writes and optimistic
  concurrency checks
- `[[wikilinks]]`, backlinks, missing-link flags and outgoing-link resolution
- Full-text search over the vault using SQLite FTS5 and BM25 ranking
- Obsidian-style editor with CodeMirror source view, live preview, formatting
  toolbar and wikilink autocomplete
- OpenAI-compatible LLM provider settings with API keys encrypted at rest
- Vault-grounded streaming chat and inline editor actions (next-line
  suggestions, grammar, rewrite, links, tags)
- File drive with folders, drag-and-drop uploads, ranged downloads, grid/list
  browsing, recent files and search
- Content-addressed blob storage with duplicate detection and orphan cleanup
- Metadata and text extraction for images, PDFs, Office/ODF/EPUB documents,
  audio and text/code files
- Automatic image descriptions through a configured vision model
- Revocable, optionally expiring public share links for files and folders,
  including folder zip downloads
- Single-container Docker deployment with a persistent `/data` volume
- Companion Android backup app (`android/`) that mirrors phone folders to the
  Nimbo file drive over a device token
- CI for the Rust backend, Svelte frontend and Android debug APK

### Security

- Cross-origin mutating requests are rejected
- Vault paths are validated and resolved under the vault root; only `.md` files
  are addressable
- Uploaded files are stored by SHA-256 content hash rather than user-supplied
  paths
- Share tokens are 256-bit random values; revoked and expired links return 410
- Public share responses never expose extracted text, metadata, EXIF/GPS or AI
  descriptions
- Inline previews are limited to safe raster images, PDF, audio, video and plain
  text; HTML, SVG and unknown types are forced to download

[Unreleased]: https://github.com/barrydeen/nimbo/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/barrydeen/nimbo/releases/tag/v0.1.0
