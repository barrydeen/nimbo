-- Device API tokens for native clients (Android backup app, CLIs).
-- Raw tokens are shown once at creation; only the SHA-256 hash is stored.
CREATE TABLE IF NOT EXISTS device_tokens (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    expires_at   INTEGER,
    revoked_at   INTEGER
);

CREATE INDEX IF NOT EXISTS idx_device_tokens_user ON device_tokens(user_id);
CREATE INDEX IF NOT EXISTS idx_device_tokens_hash ON device_tokens(token_hash);
