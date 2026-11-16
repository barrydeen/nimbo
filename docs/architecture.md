# Architecture

Nimbo is a single Rust binary that serves a Svelte SPA and a JSON API. It is
designed to be deployed as one container with one persistent volume.

## Principles

1. **The vault is the source of truth.** Documents are plain `.md` files under
   `$NIMBO_DATA_DIR/vault`. SQLite and the search index are derived and can be
   rebuilt from disk.
2. **Everything sensitive is encrypted at rest.** Provider API keys are stored
   with XChaCha20-Poly1305 using an instance secret generated on first boot.
3. **No external services.** SQLite (with FTS5) is embedded; there is no Redis,
   Postgres or Meilisearch requirement for a single-instance deployment.
4. **Fail closed.** Unknown API routes return JSON 404, mutating cross-origin
   requests are rejected, and document paths are validated before touching disk.

## Components

```
backend/src
  main.rs        router, middleware, static SPA fallback
  auth.rs        Argon2id, sessions, origin checks, rate limiting
  crypto.rs      instance secret + authenticated encryption
  db.rs          SQLite pool, migrations, settings
  agent.rs       OpenAI-compatible client, SSE streaming, context gathering
  routes/        setup, auth, docs, search, settings, agent
frontend/src
  routes/        setup, login, vault, settings
  lib/           API client and CodeMirror editor component
```

## Data flow

- **Setup:** a fresh instance has `setup_complete=false`. `POST /api/setup`
  creates the first admin inside a transaction, flips the flag, and issues a
  session.
- **Documents:** saves validate the path, check the supplied content hash against
  the file on disk (optimistic concurrency), write atomically via temp file +
  rename, then reindex.
- **Indexing:** the document row, wikilink graph and FTS5 rows are updated in the
  same request. Dangling links are re-resolved when new documents appear.
- **Search:** FTS5 with BM25 ranking and `snippet()` highlighting. Queries are
  sanitized into prefix terms to avoid FTS syntax errors.
- **Agent:** the user message is used to retrieve relevant documents (FTS), which
  are injected as system context. The provider request is streamed back to the
  browser over SSE. Inline actions use the same provider client without tools.

## Threat model notes

- Session tokens are random 256-bit values; only their SHA-256 hashes are stored.
- Cookies are `HttpOnly` and `SameSite=Strict`; set `NIMBO_SECURE_COOKIES=true`
  behind TLS.
- Path traversal is prevented lexically (`..`, absolute paths, non-`.md`) and
  files are always joined under the vault root.
- The agent is given read-only vault context in this version; write actions are
  user-initiated from the UI.

## Known limitations

- No file watcher yet: external edits are picked up on restart or when the file
  is saved through Nimbo.
- No CRDT collaboration: concurrent editors get a 409 instead of a merge.
- Search is lexical only; semantic retrieval is planned.
