-- File-tree and blob-lifecycle hardening.
--
-- The application serialises every tree/blob mutation through one process-wide
-- lock and performs reference creation in the same transaction as blob/row
-- registration. These schema constraints are defence in depth for bugs and for
-- any future code path that forgets the lock.

-- Retryable blob deletion records. A row/file is only forgotten after the
-- filesystem unlink succeeds, so operational failures cannot leak untracked
-- files.
CREATE TABLE IF NOT EXISTS blob_tombstones (
    sha256    TEXT PRIMARY KEY,
    queued_at INTEGER NOT NULL
);

-- Bounded retry accounting for the indexing/vision queues.
ALTER TABLE nodes ADD COLUMN index_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE nodes ADD COLUMN vision_attempts INTEGER NOT NULL DEFAULT 0;

-- Parent validity on insert: every non-root node must have an existing folder
-- parent. This removes the orphan-insert class even if a check-then-act gap is
-- reintroduced elsewhere.
CREATE TRIGGER IF NOT EXISTS nodes_parent_insert
BEFORE INSERT ON nodes
WHEN NEW.id <> 'root'
BEGIN
    SELECT RAISE(ABORT, 'parent folder does not exist')
    WHERE NOT EXISTS (
        SELECT 1 FROM nodes p WHERE p.id = NEW.parent_id AND p.kind = 'folder'
    );
    SELECT RAISE(ABORT, 'a node cannot be its own parent')
    WHERE NEW.parent_id = NEW.id;
END;

-- Parent validity and cycle prevention on move. The recursive walk is
-- cycle-safe (visited set) and bounded, and rejects moving a folder beneath
-- itself even when two concurrent moves would each have passed an earlier
-- application-level check.
CREATE TRIGGER IF NOT EXISTS nodes_parent_update
BEFORE UPDATE OF parent_id ON nodes
WHEN NEW.id <> 'root'
BEGIN
    SELECT RAISE(ABORT, 'parent folder does not exist')
    WHERE NOT EXISTS (
        SELECT 1 FROM nodes p WHERE p.id = NEW.parent_id AND p.kind = 'folder'
    );
    SELECT RAISE(ABORT, 'a node cannot be its own parent')
    WHERE NEW.parent_id = NEW.id;
    SELECT RAISE(ABORT, 'a folder cannot be moved inside itself')
    WHERE EXISTS (
        WITH RECURSIVE descendants(id, visited) AS (
            SELECT NEW.id, ',' || NEW.id || ','
            UNION ALL
            SELECT n.id, descendants.visited || n.id || ','
            FROM nodes n JOIN descendants ON n.parent_id = descendants.id
            WHERE instr(descendants.visited, ',' || n.id || ',') = 0
              AND length(descendants.visited) < 40000
        )
        SELECT 1 FROM descendants WHERE id = NEW.parent_id
    );
END;

-- Name and metadata validity. Kept as separate triggers so a rename does not
-- need to revalidate (possibly legacy) metadata and vice versa.
CREATE TRIGGER IF NOT EXISTS nodes_name_insert
BEFORE INSERT ON nodes
BEGIN
    SELECT RAISE(ABORT, 'name is too long')
    WHERE length(CAST(NEW.name AS BLOB)) > 255;
    SELECT RAISE(ABORT, 'name is required')
    WHERE NEW.id <> 'root' AND length(NEW.name) = 0;
    SELECT RAISE(ABORT, 'name may not contain path separators')
    WHERE instr(NEW.name, '/') > 0 OR instr(NEW.name, char(92)) > 0;
    SELECT RAISE(ABORT, 'name may not contain a null byte')
    WHERE instr(NEW.name, char(0)) > 0;
END;

CREATE TRIGGER IF NOT EXISTS nodes_name_update
BEFORE UPDATE OF name ON nodes
WHEN NEW.id <> 'root'
BEGIN
    SELECT RAISE(ABORT, 'name is too long')
    WHERE length(CAST(NEW.name AS BLOB)) > 255;
    SELECT RAISE(ABORT, 'name is required')
    WHERE length(NEW.name) = 0;
    SELECT RAISE(ABORT, 'name may not contain path separators')
    WHERE instr(NEW.name, '/') > 0 OR instr(NEW.name, char(92)) > 0;
    SELECT RAISE(ABORT, 'name may not contain a null byte')
    WHERE instr(NEW.name, char(0)) > 0;
END;

CREATE TRIGGER IF NOT EXISTS nodes_metadata_insert
BEFORE INSERT ON nodes
BEGIN
    SELECT RAISE(ABORT, 'metadata must be valid json')
    WHERE json_valid(NEW.metadata_json) = 0;
    SELECT RAISE(ABORT, 'metadata is too large')
    WHERE length(CAST(NEW.metadata_json AS BLOB)) > 65536;
    SELECT RAISE(ABORT, 'size must not be negative')
    WHERE NEW.size < 0;
END;

CREATE TRIGGER IF NOT EXISTS nodes_metadata_update
BEFORE UPDATE OF metadata_json, size ON nodes
BEGIN
    SELECT RAISE(ABORT, 'metadata must be valid json')
    WHERE json_valid(NEW.metadata_json) = 0;
    SELECT RAISE(ABORT, 'metadata is too large')
    WHERE length(CAST(NEW.metadata_json AS BLOB)) > 65536;
    SELECT RAISE(ABORT, 'size must not be negative')
    WHERE NEW.size < 0;
END;
