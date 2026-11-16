-- Scoped device tokens. Existing tokens remain full-access; a backup token is
-- bound to the root child folder discovered/created at token creation time.
ALTER TABLE device_tokens ADD COLUMN scope TEXT NOT NULL DEFAULT 'full';
ALTER TABLE device_tokens ADD COLUMN backup_root_id TEXT;

CREATE INDEX IF NOT EXISTS idx_device_tokens_scope ON device_tokens(scope);
