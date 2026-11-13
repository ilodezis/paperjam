-- Passkeys (WebAuthn credentials) that sign a user in to the web UI. Only the
-- public half is stored: `passkey` is the serialized webauthn-rs credential
-- (public key, algorithm, flags), `sign_count` mirrors its signature counter so
-- a counter that goes backwards can be refused with a plain comparison.

CREATE TABLE passkeys (
    id            TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    credential_id TEXT NOT NULL UNIQUE,
    passkey       TEXT NOT NULL,
    sign_count    INTEGER NOT NULL DEFAULT 0,
    label         TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    last_used_at  TEXT
);

CREATE INDEX passkeys_user ON passkeys (user_id);
