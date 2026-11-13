-- A clean copy of a scanned document: its pages upright, cut out of the sheet
-- and straightened, as one PDF next to the original (derived/<id>/clean.pdf).
-- Its size in bytes; NULL when the document has none (born-digital PDFs,
-- text, documents not processed since the copy was introduced).
ALTER TABLE files ADD COLUMN clean_size INTEGER;

-- A page that shows one document to anyone holding its link, for a short
-- while. The link carries the id and a signature of it, so the row holds no
-- secret and the link can be shown again while it is live.
CREATE TABLE shares (
    id           TEXT PRIMARY KEY,
    document_id  TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    created_by   TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at   TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    revoked_at   TEXT,
    -- Whether the page shows the summary and the extracted values, or only
    -- the title, type and date next to the pages.
    show_details INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX shares_document ON shares (document_id, expires_at);
