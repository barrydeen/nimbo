# Changelog

All notable changes to Nimbo are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned

- File watcher so external vault edits are reindexed without a restart
- Semantic search with chunking, embeddings and hybrid BM25 + vector ranking
- Agent tool calls with confirm-before-apply writes
- Resumable uploads and background folder-zip jobs
- Backup/restore tooling and published release images

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
