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
3. **Provider API keys are encrypted at rest; notes and files are not.**
   Provider settings are stored with XChaCha20-Poly1305 using an instance secret
   generated on first boot. Markdown notes, SQLite rows, sessions, uploaded
   blobs, thumbnails and backups are plaintext. Use host/filesystem encryption
   and protected backup storage when disk theft or backup exposure is in scope.
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
    extract.rs   MIME sniffing, text decoding, image/EXIF/PDF/Office/audio
                 metadata and optional ffmpeg video frame thumbnails
    vision.rs    image downscaling/orientation and description persistence
    worker.rs    background indexing and vision queues
  routes/        setup, auth, docs, search, settings, agent, files, shares, public
frontend/src
  routes/        setup, login, vault, drive, s/[token], settings
  lib/           API client, upload helper and CodeMirror editor component
```

## Data flow

- **Setup:** a fresh instance has `setup_complete=false`. When
  `NIMBO_SETUP_TOKEN` is configured, `POST /api/setup` requires that one-time
  value and compares it in constant time. The first successful request creates
  the administrator inside a transaction and flips the flag. A completed
  instance rejects setup before password hashing.
- **Authentication:** browsers use an HttpOnly session cookie. Native clients
  send `Authorization: Bearer nmb_…`; device tokens are looked up by SHA-256
  hash, checked for revocation/expiry, and touch `last_used_at` on use. Session
  tokens have full scope. Device tokens default to full scope for backward
  compatibility; backup-scoped tokens are confined to the `Phone Backups`
  subtree and cannot use docs, agent, settings, shares or token administration.
- **Documents:** saves validate the path, check the supplied content hash against
  the file on disk (optimistic concurrency), write atomically via a unique temp
  file + no-replace rename, then update the index from the committed bytes. The
  decoded content is capped at 8 MiB; the route accepts a 16 MiB JSON body so
  escaping overhead does not disconnect the advertised note limit.
- **Indexing:** the document row, wikilink graph and FTS5 rows are updated in the
  same request. Dangling links are re-resolved when new documents appear.
- **Search:** FTS5 with BM25 ranking and `snippet()` highlighting. Queries are
  sanitized into prefix terms to avoid FTS syntax errors.
- **Agent:** the document-agent model runs a bounded tool-calling loop over the
  vault. Read-only tools list/search/read documents and search files; draft tools
  (`propose_edit`, `append_to_document`, `replace_document`, `create_document`)
  validate exact anchors against the authoritative editor buffer or on-disk note
  and emit reviewable draft events. Draft tools never write: the browser applies
  accepted diffs through the normal optimistic-concurrency save endpoint, so 409
  conflicts and native editor undo still work. The assistant receives the current
  buffer, selection, backlinks, vault index, pending draft status and relevant FTS
  hits as bounded context. Streaming SSE carries text, tool-step and draft events;
  providers without tool support fall back to a tagged `nimbo-draft` JSON fence.
  The loop caps rounds, tool calls, drafts, context and total runtime. Inline
  editor actions (next-line completion, grammar, rewrite, links, tags) use the
  same document-agent provider client without tools. Providers and model roles are
  configured separately in Settings; role model references are validated against
  each provider's selected model allowlist.
- **File uploads:** multipart chunks are streamed to a temp file while hashing.
  The completed blob is promoted into the content-addressed store, and a node row
  is inserted with a sibling-unique name in a lifecycle transaction. Duplicate
  names are suffixed (`file (2).ext`). The request is capped by
  `NIMBO_MAX_UPLOAD_MB` per file and by roughly that value plus 1 MiB for the
  whole multipart request. `PUT /api/files/{id}/content` replaces the bytes of an
  existing file node while preserving its identity and name.
- **File indexing:** a background worker claims `index_status='pending'` rows,
  extracts text/metadata/thumbnails in a blocking task, stores the thumbnail as
  another blob, and refreshes the file's FTS row. Images are decoded in-process;
  videos use the optional `ffmpeg` binary to extract one bounded JPEG frame with
  its own CPU/address-space limits. Parsing applies byte, entry, pixel and
  expansion caps before dispatch and each job has a five-minute deadline, but
  native parser work may continue after a deadline; container limits remain the
  hard backstop.
- **Remote browsing:** `GET /api/files/media` returns a flat, paginated,
  newest-first image/video listing scoped like every other file query. The
  Android app uses it for the gallery, `GET /api/files/search` for drive-wide
  search, and the read-only `/api/docs/tree` + `/api/docs/content` routes for
  the vault. Authenticated thumbnails, inline image content and downloads all
  carry the device token; no public share link is created for the app.
- **Image descriptions:** images with `vision_status='pending'` are downscaled
  (EXIF orientation applied), sent to the model assigned to the file-indexing role
  (falling back to the document-agent model when no indexing model is set), and
  stored as description, visible text and tags. Missing providers mark images
  `skipped`; saving LLM settings requeues skipped images. Failures require a
  manual retry.
- **Shares:** share IDs are 256-bit random tokens. Public endpoints resolve the
  token, reject revoked/expired links with 410, and verify every requested node is
  the shared file or inside the shared folder. Folder zips are built in a temp
  file and streamed, capped per archive at 10,000 entries / 4 GiB, with at most
  two concurrent builds and an 8 GiB aggregate staging budget. They still
  consume shared disk and CPU and require operator headroom.

## Threat model notes

- Session tokens are random 256-bit values; only their SHA-256 hashes are stored.
- Device tokens are random 256-bit `nmb_…` values, stored only as SHA-256
  hashes. They are revocable. Full-scope tokens inherit the creator's
  permissions; backup-scope tokens are confined to the `Phone Backups` subtree.
- Cookies are `HttpOnly` and `SameSite=Strict`; set `NIMBO_SECURE_COOKIES=true`
  behind TLS. TLS is not provided by that flag and must be terminated by a
  reverse proxy or equivalent.
- First-run setup is claimable by the first reachable client unless
  `NIMBO_SETUP_TOKEN` is set. Keep the default loopback publication or complete
  setup through a tunnel/TLS before exposing the service.
- Path traversal is prevented lexically (`..`, absolute paths, non-`.md`) and
  files are always joined under the vault root, with descriptor-relative checks
  for final and parent symlinks.
- Agent tools are read-only except for draft proposals. Draft proposals are
  validated but never persisted by the agent; only an explicit Apply in the UI
  saves through the normal document endpoint with `base_hash` concurrency checks.
  Vault and file content is delimited as untrusted context, not instructions, and
  the tool loop is bounded in rounds, calls, context, drafts and wall-clock time.
- Public share responses expose only names, sizes, types and preview/download
  URLs. Extracted text, EXIF/GPS, document metadata and AI descriptions are
  available only to authenticated users.
- Inline rendering is restricted to raster images, PDF, audio, video and plain
  text/Markdown/JSON/CSV. HTML, SVG, XML and unknown types are served as
  attachments with `X-Content-Type-Options: nosniff`. Markdown preview and
  search snippets are sanitized in the browser.
- Uploaded data is untrusted input: extraction applies input-size, entry,
  pixel and expansion caps, and each background job has a five-minute deadline.
  Blocking parser work cannot be safely preempted by dropping a task handle, so
  container CPU/memory limits remain the hard backstop. Password-protected or
  malformed files are marked partial rather than failing the upload.
- Only provider API keys are encrypted by the application (`secret.key`).
  Notes, database contents, sessions, blobs and backups are plaintext; protect
  the volume and backups with host/filesystem encryption.
- When automatic descriptions are enabled, image bytes leave the instance for the
  configured provider. Operators who need local-only storage should disable the
  feature or point the provider at a local vision model.

## Known limitations

- No file watcher yet: external edits are picked up on restart or when the file
  is saved through Nimbo.
- No CRDT collaboration: concurrent editors get a 409 instead of a merge.
- Search is lexical only; semantic retrieval is planned.
- File uploads are not resumable. Folder zips are generated synchronously before
  the response starts, so very large folders can take a while and are capped per
  archive.
- All authenticated users have full access to the file drive; there are no
  per-user or per-folder permissions beyond the coarse backup-token scope.
- Notes, database rows, blobs and backups are not encrypted at rest by the
  application. Only provider API keys use the instance key. Host/filesystem and
  backup encryption are operator responsibilities.
- Parser work is bounded by input caps and a five-minute job deadline, but native
  parser code cannot be safely preempted by dropping an async task handle.
  Container CPU/memory limits remain the hard backstop against a hostile parser.
- Backup/restore is a documented offline runbook, not a built-in scheduling or
  point-in-time recovery service. There is no supported online backup mode or
  multi-replica deployment.
- Expired sessions and audit rows older than `NIMBO_AUDIT_RETENTION_DAYS`
  (default 90) are purged at startup and periodically. Container logs rotate in
  the supplied Compose files; external log collectors need their own retention.
- Android backups are constrained to unmetered networks, which is not the same
  as a specific Wi-Fi network. Plain HTTP requires an explicit insecure-allow
  setting and exposes a device credential; use HTTPS.
- Public folder archives stage data on the same volume as the database. Per
  archive, concurrency and aggregate staging are capped (10,000 entries / 4 GiB,
  two builds, 8 GiB aggregate), but there is no per-operator quota.
