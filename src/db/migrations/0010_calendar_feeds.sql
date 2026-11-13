-- A calendar subscription of one user: the due dates of the archive as an .ics
-- feed that Apple, Google and Outlook calendars poll. The link carries a
-- random secret that opens this feed only (never the API); the row keeps its
-- SHA-256 hash, so the link is shown once, when it is made or rotated.
CREATE TABLE calendar_feeds (
    user_id      TEXT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    secret_hash  TEXT NOT NULL UNIQUE,
    created_at   TEXT NOT NULL,
    last_used_at TEXT
);
