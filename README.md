# Nimbo

Self-hosted, MIT-licensed markdown knowledge base with first-class search and a
built-in agent. Documents are plain `.md` files on disk, so your vault is yours
and works with any editor.

> Status: early but usable. Auth, first-run setup, document CRUD, wikilinks,
> full-text search, LLM provider settings and an agent layer (vault-grounded chat
> plus inline writing actions) are implemented.

## What works today

- First-run setup wizard that creates the administrator account
- Argon2id passwords, HttpOnly `SameSite=Strict` sessions, login rate limiting
- Document module: create/read/update/delete `.md` files in a vault directory
- `[[wikilinks]]` with backlinks, outgoing-link resolution and missing-link flags
- Optimistic concurrency: saves are rejected if the file changed underneath you
- Full-text search over the vault with ranked snippets (SQLite FTS5/BM25)
- Obsidian-style editor: CodeMirror source view, live preview, wikilink autocomplete
- Agent: connect any OpenAI-compatible provider (OpenAI, Ollama, LM Studio, vLLM,
  OpenRouter); API keys encrypted at rest
- Vault-grounded streaming chat that retrieves relevant documents before answering
- Inline actions: continue writing, grammar, rewrite, link suggestions, tags
- Single-container Docker deployment with a persistent `/data` volume

## Quick start (Docker)

```bash
cd docker
docker compose up --build
```

Open <http://localhost:8080>. The first visit sends you to the setup wizard.
Create the admin account, then start writing.

Data lives in the `nimbo_data` volume:

```
/data/nimbo.db      SQLite database (users, sessions, index)
/data/secret.key    instance encryption secret (keep it with your backups)
/data/vault/        your markdown files
```

Back up `/data` as a whole. If `nimbo.db` is lost the index can be rebuilt from
the vault on startup; `secret.key` must be preserved to decrypt future secrets.

## Local development

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

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `NIMBO_BIND` | `0.0.0.0:8080` | Listen address |
| `NIMBO_DATA_DIR` | `./data` | Database, secret and vault location |
| `NIMBO_STATIC_DIR` | unset | Serve the built frontend from this directory |
| `NIMBO_SECURE_COOKIES` | `false` | Mark session cookies `Secure` (use behind HTTPS) |
| `NIMBO_SESSION_TTL_DAYS` | `30` | Session lifetime in days |

## Security notes

- Put Nimbo behind a TLS-terminating reverse proxy and set
  `NIMBO_SECURE_COOKIES=true`.
- Cross-origin mutating requests are rejected by comparing `Origin` to `Host`.
- Path traversal is blocked: every document path is validated and resolved under
  the vault root, and only `.md` files are addressable.
- Uploaded content is capped at 8 MiB per document.

## API

All routes are JSON. Mutations require the session cookie.

```
POST /api/setup                 create admin (first run only)
POST /api/auth/login            sign in
POST /api/auth/logout           sign out
GET  /api/bootstrap             setup state + current user
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
GET  /healthz                   health check
```

## Roadmap

1. File watcher so external edits are reindexed without a restart.
2. Semantic search: chunking, embeddings, hybrid BM25 + vector ranking.
3. Agent tool calls (search/read/write documents) with confirm-before-apply.
4. Hardening: backups/restore tooling, upgrade path, release images.

## License

MIT. See [LICENSE](LICENSE).
