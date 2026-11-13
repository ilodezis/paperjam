-- paperjam initial schema.
-- Conventions: ids are UUIDv7 TEXT, timestamps are RFC 3339 UTC TEXT, JSON
-- columns are TEXT. `provenance` is one of llm|mrz|manual|local.

CREATE TABLE people (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    aliases    TEXT NOT NULL DEFAULT '[]',
    notes      TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    locale        TEXT NOT NULL DEFAULT 'en',
    person_id     TEXT REFERENCES people (id) ON DELETE SET NULL,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    disabled_at   TEXT
);

CREATE TABLE api_tokens (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    prefix       TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at   TEXT
);

CREATE TABLE sessions (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX sessions_user ON sessions (user_id);

CREATE TABLE correspondents (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    aliases    TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL
);

CREATE TABLE tags (
    id    TEXT PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE COLLATE NOCASE,
    color TEXT
);

-- `labels` is {"en": "...", "ru": "..."}; `fields` is
-- [{key, labels, kind, validator?, required}].
CREATE TABLE document_types (
    id         TEXT PRIMARY KEY,
    key        TEXT NOT NULL UNIQUE,
    labels     TEXT NOT NULL,
    fields     TEXT NOT NULL DEFAULT '[]',
    builtin    INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE documents (
    id                      TEXT PRIMARY KEY,
    title                   TEXT,
    title_provenance        TEXT CHECK (title_provenance IN ('llm', 'mrz', 'manual', 'local')),
    document_date           TEXT,
    date_provenance         TEXT CHECK (date_provenance IN ('llm', 'mrz', 'manual', 'local')),
    type_id                 TEXT REFERENCES document_types (id) ON DELETE SET NULL,
    type_provenance         TEXT CHECK (type_provenance IN ('llm', 'mrz', 'manual', 'local')),
    correspondent_id        TEXT REFERENCES correspondents (id) ON DELETE SET NULL,
    correspondent_provenance TEXT CHECK (correspondent_provenance IN ('llm', 'mrz', 'manual', 'local')),
    summary                 TEXT,
    summary_provenance      TEXT CHECK (summary_provenance IN ('llm', 'mrz', 'manual', 'local')),
    language                TEXT,
    local_only              INTEGER NOT NULL DEFAULT 0,
    status                  TEXT NOT NULL DEFAULT 'queued'
                            CHECK (status IN ('queued', 'processing', 'ready', 'failed')),
    status_reason           TEXT,
    attention               TEXT NOT NULL DEFAULT '[]',
    added_by                TEXT REFERENCES users (id) ON DELETE SET NULL,
    updated_by              TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    trashed_at              TEXT
);
CREATE INDEX documents_status ON documents (status);
CREATE INDEX documents_type ON documents (type_id);
CREATE INDEX documents_correspondent ON documents (correspondent_id);
CREATE INDEX documents_trashed ON documents (trashed_at);

CREATE TABLE document_people (
    document_id TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    provenance  TEXT NOT NULL CHECK (provenance IN ('llm', 'mrz', 'manual', 'local')),
    PRIMARY KEY (document_id, person_id)
);
CREATE INDEX document_people_person ON document_people (person_id);

CREATE TABLE document_tags (
    document_id TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    tag_id      TEXT NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    provenance  TEXT NOT NULL CHECK (provenance IN ('llm', 'mrz', 'manual', 'local')),
    PRIMARY KEY (document_id, tag_id)
);
CREATE INDEX document_tags_tag ON document_tags (tag_id);

-- The original upload. A purge deletes the row, so a plain unique index on
-- the hash is "unique among non-purged" and dedup needs no extra state.
CREATE TABLE files (
    document_id   TEXT PRIMARY KEY REFERENCES documents (id) ON DELETE CASCADE,
    sha256        TEXT NOT NULL,
    mime          TEXT NOT NULL,
    size          INTEGER NOT NULL,
    original_name TEXT NOT NULL,
    page_count    INTEGER,
    storage_key   TEXT NOT NULL
);
CREATE UNIQUE INDEX files_sha256 ON files (sha256);

CREATE TABLE pages (
    document_id       TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    n                 INTEGER NOT NULL,
    width             INTEGER,
    height            INTEGER,
    has_embedded_text INTEGER NOT NULL DEFAULT 0,
    render_key        TEXT,
    PRIMARY KEY (document_id, n)
);

CREATE TABLE text_layers (
    document_id   TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    page          INTEGER NOT NULL,
    source        TEXT NOT NULL CHECK (source IN ('embedded', 'tesseract', 'llm')),
    text          TEXT NOT NULL,
    word_boxes    BLOB,
    quality_score REAL,
    created_at    TEXT NOT NULL,
    PRIMARY KEY (document_id, page, source)
);

CREATE TABLE field_values (
    document_id TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    field_key   TEXT NOT NULL,
    value       TEXT NOT NULL,
    provenance  TEXT NOT NULL CHECK (provenance IN ('llm', 'mrz', 'manual', 'local')),
    page        INTEGER,
    updated_by  TEXT REFERENCES users (id) ON DELETE SET NULL,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (document_id, field_key)
);

CREATE TABLE facts (
    document_id TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,
    provenance  TEXT NOT NULL CHECK (provenance IN ('llm', 'mrz', 'manual', 'local')),
    page        INTEGER
);
CREATE INDEX facts_document ON facts (document_id);

-- Usage outlives the document: purging a document must not erase spend.
CREATE TABLE llm_usage (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id   TEXT REFERENCES documents (id) ON DELETE SET NULL,
    model         TEXT NOT NULL,
    step          TEXT NOT NULL,
    input_tokens  INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd      TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX llm_usage_created ON llm_usage (created_at);

-- Append-only audit log. Deliberately no foreign keys: events outlive
-- purged documents and disabled users.
CREATE TABLE events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    actor_user_id TEXT,
    via           TEXT NOT NULL,
    action        TEXT NOT NULL,
    document_id   TEXT,
    at            TEXT NOT NULL,
    diff          TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX events_document ON events (document_id);

CREATE TABLE jobs (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    document_id TEXT REFERENCES documents (id) ON DELETE CASCADE,
    payload     TEXT NOT NULL DEFAULT '{}',
    state       TEXT NOT NULL DEFAULT 'queued'
                CHECK (state IN ('queued', 'running', 'done', 'dead')),
    attempts    INTEGER NOT NULL DEFAULT 0,
    run_after   TEXT NOT NULL,
    last_error  TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX jobs_claim ON jobs (state, run_after);
CREATE INDEX jobs_document ON jobs (document_id);

CREATE TABLE oauth_clients (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    redirect_uris TEXT NOT NULL,
    secret_hash   TEXT,
    approved_by   TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at    TEXT NOT NULL,
    last_used_at  TEXT,
    revoked_at    TEXT
);

CREATE TABLE oauth_grants (
    id         TEXT PRIMARY KEY,
    client_id  TEXT NOT NULL REFERENCES oauth_clients (id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE upload_links (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at TEXT NOT NULL,
    used_at    TEXT
);
