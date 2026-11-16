# Architecture

Nimbo is a single Rust binary that serves a Svelte SPA and a JSON API. It is
designed to be deployed as one container with one persistent volume.

## Principles

1. **The vault is the source of truth for notes.** Documents are plain `.md`
   files under `$NIMBO_DATA_DIR/vault`. SQLite and the document index are derived
   and can be rebuilt from disk.
2. **Files are content-addressed.** Uploaded bytes live under
   `$NIMBO_DATA_DIR/files/blobs/<aa>/<bb>/<sha256>`. The virtual folder tree,
   share links and extraction results live in SQLite. Identical uploads share one
   blob.
3. **Everything sensitive is encrypted at rest.** Provider API keys are stored
   with XChaCha20-Poly1305 using an instance secret generated on first boot.
4. **No external services.** SQLite (with FTS5) is embedded; there is no Redis,
   Postgres or Meilisearch requirement for a single-instance deployment.
5. **Fail closed.** Unknown API routes return JSON 404, mutating cross-origin
   requests are rejected, document paths are validated before touching disk, and
   public share lookups are checked against the shared subtree.

## Components

```
backend/src
  main.rs        router, middleware, static SPA fallback
  auth.rs        Argon2id, sessions, device tokens, origin checks, rate limiting
  crypto.rs      instance secret + authenticated encryption
  db.rs          SQLite pool, migrations, settings
  agent.rs       OpenAI-compatible client, SSE streaming, context gathering,
                 vision descriptions
  files/
    mod.rs       node tree, name rules, breadcrumbs, containment, FTS upserts
    storage.rs   content-addressed blob paths, temp promotion, orphan GC
    extract.rs   MIME sniffing, text decoding, image/EXIF/PDF/Office/audio metadata
    vision.rs    image downscaling/orientation and description persistence
    worker.rs    background indexing and vision queues
  routes/        setup, auth, docs, search, settings, agent, files, shares, public
frontend/src
  routes/        setup, login, vault, drive, s/[token], settings
  lib/           API client, upload helper and CodeMirror editor component
```

## Data flow

- **Setup:** a fresh instance has `setup_complete=false`. `POST /api/setup`
  creates the first admin inside a transaction, flips the flag, and issues a
  session.
- **Authentication:** browsers use an HttpOnly session cookie. Native clients
  send `Authorization: Bearer nmb_…`; device tokens are looked up by SHA-256
  hash, checked for revocation/expiry, and touch `last_used_at` on use.
- **Documents:** saves validate the path, check the supplied content hash against
  the file on disk (optimistic concurrency), write atomically via temp file +
  rename, then reindex.
- **Indexing:** the document row, wikilink graph and FTS5 rows are updated in the
  same request. Dangling links are re-resolved when new documents appear.
- **Search:** FTS5 with BM25 ranking and `snippet()` highlighting. Queries are
  sanitized into prefix terms to avoid FTS syntax errors.
- **Agent:** the user message is used to retrieve relevant documents and files
  (FTS across names, extracted text, metadata and image descriptions), which are
  injected as system context. The provider request is streamed back to the
  browser over SSE. Inline editor actions (next-line completion, grammar, rewrite,
  links, tags) use the same provider client without tools.
- **File uploads:** multipart chunks are streamed to a temp file while hashing.
  The completed blob is promoted into the content-addressed store, and a node row
  is inserted with a sibling-unique name. Duplicate names are suffixed
  (`file (2).ext`). The request is capped by `NIMBO_MAX_UPLOAD_MB`.
- **File indexing:** a background worker claims `index_status='pending'` rows,
  extracts text/metadata/thumbnails in a blocking task, stores the thumbnail as
  another blob, and refreshes the file's FTS row.
- **Image descriptions:** images with `vision_status='pending'` are downscaled
  (EXIF orientation applied), sent to the configured vision model, and stored as
  description, visible text and tags. Missing providers mark images `skipped`;
  saving LLM settings requeues skipped images. Failures require a manual retry.
- **Shares:** share IDs are 256-bit random tokens. Public endpoints resolve the
  token, reject revoked/expired links with 410, and verify every requested node is
  the shared file or inside the shared folder. Folder zips are built in a temp
  file and streamed, capped at 10,000 entries / 4 GiB.

## Threat model notes

- Session tokens are random 256-bit values; only their SHA-256 hashes are stored.
- Device tokens are random 256-bit `nmb_…` values, stored only as SHA-256
  hashes. They are revocable and inherit the permissions of their creator.
- Cookies are `HttpOnly` and `SameSite=Strict`; set `NIMBO_SECURE_COOKIES=true`
  behind TLS.
- Path traversal is prevented lexically (`..`, absolute paths, non-`.md`) and
  files are always joined under the vault root.
- The agent is given read-only vault and file context in this version; write
  actions are user-initiated from the UI.
- Public share responses expose only names, sizes, types and preview/download
  URLs. Extracted text, EXIF/GPS, document metadata and AI descriptions are
  available only to authenticated users.
- Inline rendering is restricted to raster images, PDF, audio, video and plain
  text/Markdown/JSON/CSV. HTML, SVG, XML and unknown types are served as
  attachments with `X-Content-Type-Options: nosniff`.
- Uploaded data is untrusted input: extraction runs with size/time budgets and
  parsers are isolated in blocking tasks. Password-protected or malformed files
  are marked partial rather than failing the upload.
- When automatic descriptions are enabled, image bytes leave the instance for the
  configured provider. Operators who need local-only storage should disable the
  feature or point the provider at a local vision model.

## Known limitations

- No file watcher yet: external edits are picked up on restart or when the file
  is saved through Nimbo.
- No CRDT collaboration: concurrent editors get a 409 instead of a merge.
- Search is lexical only; semantic retrieval is planned.
- File uploads are not resumable. Folder zips are generated synchronously before
  the response starts, so very large folders can take a while and are capped.
- All authenticated users have full access to the file drive; there are no
  per-user or per-folder permissions yet.
