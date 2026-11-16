CREATE TABLE IF NOT EXISTS blobs (
    sha256     TEXT PRIMARY KEY,
    size       INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

-- Unified file/folder tree. The root row has id 'root' and parent_id ''.
CREATE TABLE IF NOT EXISTS nodes (
    id            TEXT PRIMARY KEY,
    parent_id     TEXT NOT NULL,
    kind          TEXT NOT NULL CHECK (kind IN ('folder', 'file')),
    name          TEXT NOT NULL,
    owner_id      TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,

    blob_sha      TEXT,
    mime          TEXT,
    size          INTEGER NOT NULL DEFAULT 0,
    thumb_sha     TEXT,

    index_status  TEXT NOT NULL DEFAULT 'pending',
    index_error   TEXT,
    text_content  TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',

    vision_status      TEXT NOT NULL DEFAULT 'n/a',
    vision_description TEXT,
    vision_text        TEXT,
    vision_tags        TEXT,
    vision_model       TEXT,
    vision_at          INTEGER,
    vision_error       TEXT
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_sibling_name
    ON nodes(parent_id, name COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_nodes_parent ON nodes(parent_id);
CREATE INDEX IF NOT EXISTS idx_nodes_blob ON nodes(blob_sha);
CREATE INDEX IF NOT EXISTS idx_nodes_thumb ON nodes(thumb_sha);
CREATE INDEX IF NOT EXISTS idx_nodes_index_status ON nodes(index_status);
CREATE INDEX IF NOT EXISTS idx_nodes_vision_status ON nodes(vision_status);

INSERT OR IGNORE INTO nodes (id, parent_id, kind, name, created_at, updated_at)
VALUES ('root', '', 'folder', '', 0, 0);

CREATE TABLE IF NOT EXISTS shares (
    id               TEXT PRIMARY KEY,
    target_type      TEXT NOT NULL CHECK (target_type IN ('file', 'folder')),
    target_id        TEXT NOT NULL,
    created_by       TEXT,
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER,
    revoked_at       INTEGER,
    last_accessed_at INTEGER,
    access_count     INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_shares_target ON shares(target_id);
CREATE INDEX IF NOT EXISTS idx_shares_token_lookup ON shares(id);

CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
    file_id UNINDEXED,
    name,
    text,
    description,
    metadata,
    tokenize = 'porter unicode61'
);
