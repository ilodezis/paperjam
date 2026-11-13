-- Authorization codes and refresh tokens of the OAuth server. Only hashes are
-- stored; a code or refresh token is shown to the client once.

CREATE TABLE oauth_codes (
    code_hash      TEXT PRIMARY KEY,
    grant_id       TEXT NOT NULL REFERENCES oauth_grants (id) ON DELETE CASCADE,
    redirect_uri   TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    expires_at     TEXT NOT NULL,
    used_at        TEXT
);

CREATE INDEX oauth_codes_grant ON oauth_codes (grant_id);

CREATE TABLE oauth_refresh_tokens (
    token_hash TEXT PRIMARY KEY,
    grant_id   TEXT NOT NULL REFERENCES oauth_grants (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    used_at    TEXT
);

CREATE INDEX oauth_refresh_tokens_grant ON oauth_refresh_tokens (grant_id);
