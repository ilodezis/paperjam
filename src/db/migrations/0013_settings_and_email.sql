CREATE TABLE app_settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    encrypted  INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    updated_by TEXT REFERENCES users (id) ON DELETE SET NULL
);

CREATE TABLE email_intake (
    id               INTEGER PRIMARY KEY CHECK (id = 1),
    enabled          INTEGER NOT NULL DEFAULT 0,
    secret_hash      TEXT,
    secret_prefix    TEXT,
    user_id          TEXT REFERENCES users (id) ON DELETE SET NULL,
    allowed_senders  TEXT NOT NULL DEFAULT '[]',
    body_fallback    INTEGER NOT NULL DEFAULT 1,
    local_only       INTEGER NOT NULL DEFAULT 0,
    tag              TEXT,
    last_received_at TEXT,
    received_count   INTEGER NOT NULL DEFAULT 0,
    updated_at       TEXT NOT NULL,
    updated_by       TEXT REFERENCES users (id) ON DELETE SET NULL
);
