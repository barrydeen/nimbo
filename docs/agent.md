# Agent assistant

The vault editor's right sidebar is a conversational assistant with a bounded
tool-calling loop. It can search and read the vault and file drive, and it can
propose changes to documents. It never writes a change without an explicit
human acceptance.

## Intent and modes

The composer offers three modes:

- **Auto** (default): the model decides whether to answer or propose a draft.
- **Ask**: draft tools are omitted; the assistant answers and can still search or
  read.
- **Edit**: the assistant is expected to produce a concrete draft, while still
  answering purely informational questions.

The system prompt gives the model explicit rules:

- questions, explanations and brainstorming stay in chat;
- concrete change requests use draft tools;
- ambiguous requests may ask one clarifying question or propose a small focused
  draft, but must not rewrite a document speculatively;
- a pending draft is discussed in chat unless the user asks for changes;
- the assistant never claims to have changed a document.

A server-side intent hint (question / edit / brainstorm / ambiguous) is included
as advisory context. `Edit` mode strongly biases the prompt toward producing a
draft, while keeping tool choice automatic so providers with partial tool support
still work and the model can ask a clarifying question when it needs to.

## Tools

| Tool | Purpose | Writes |
| --- | --- | --- |
| `list_documents` | List vault paths and titles | no |
| `search_documents` | FTS search with snippets | no |
| `read_document` | Read a document, optionally a line range | no |
| `search_files` | Search names, extracted text, metadata and vision descriptions | no |
| `propose_edit` | Propose exact unique `find` → `replace` edits | draft only |
| `append_to_document` | Propose appended markdown | draft only |
| `replace_document` | Propose a full rewrite of a short document | draft only |
| `create_document` | Propose a new markdown document | draft only |

Draft tools validate their anchors against the authoritative content: the live
editor buffer when the request includes one, otherwise the on-disk document.
`find` must be non-empty, occur exactly once, and edits must not overlap.

## Draft lifecycle

A draft event carries an id, kind, path, rationale, `base_hash`, the client's
`base_version` and either edit hunks or new content. The UI renders a bounded
line diff.

- **Apply** re-validates every anchor against the live buffer, dispatches one
  CodeMirror transaction (preserving native undo), then saves through
  `PUT /api/docs/content` with the last saved `base_hash`.
- **Discard** leaves the document untouched.
- **Open** jumps to the target document.
- **Undo** uses the editor history for an applied draft.
- **Regenerate** asks the assistant for a fresh draft when an anchor no longer
  matches.

A draft targeting a document that is not currently open is fetched, re-validated,
saved with that document's current hash, and the tree is refreshed. A create is
saved with `base_hash: null`, so an existing path returns a conflict.

## Streaming protocol

`POST /api/agent/chat` accepts the message, current path, live document buffer,
selection, bounded history, mode and draft summaries. It returns SSE events:

| Event | Data |
| --- | --- |
| `status` | `{"label":"Thinking…"}` |
| `text` | assistant text chunk |
| `tool` | `{"id","name","state","summary","detail"}` |
| `draft` | draft JSON |
| `done` | `{"reason":"stop\|length\|tool_limit\|timeout"}` |
| `error` | error message |

The loop is bounded to five provider rounds, sixteen tool calls, six drafts per
response, twenty edits per draft and a 300-second total runtime. Tool arguments,
tool results, context, history and draft content are all size-limited.

## Providers without tool support

Some OpenAI-compatible providers or local models do not implement tool calling.
If the provider rejects a tool request as unsupported, Nimbo retries without
tools using a fallback protocol: the model may end its reply with a fenced
````nimbo-draft`` JSON block. The backend strips the block from visible text,
validates it exactly like a tool proposal, and emits a normal draft event. In
`Ask` mode draft fences are ignored.

## Safety

- Draft tools never write to disk.
- Paths use the same lexical and containment validation as document routes.
- Vault and file content is wrapped as untrusted context; the prompt says it is
  data, not instructions.
- Saves always use optimistic concurrency; a stale draft fails closed with a
  clear message rather than silently overwriting external changes.
- Conversations and draft cards persist in `sessionStorage` per document for the
  current browser session, bounded in size. Server-side conversation storage is
  a future enhancement.
