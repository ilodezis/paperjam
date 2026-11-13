-- Who decided a document's people. `manual` means a user (or an agent) set the
-- list by hand, even when they set it to nothing, so matching never touches it.
-- Without this column a list cleared by hand is indistinguishable from a
-- document nobody has matched yet.

ALTER TABLE documents ADD COLUMN people_provenance TEXT
    CHECK (people_provenance IN ('llm', 'mrz', 'manual', 'local'));

UPDATE documents SET people_provenance = 'manual'
    WHERE EXISTS (
        SELECT 1 FROM document_people dp
        WHERE dp.document_id = documents.id AND dp.provenance = 'manual'
    );
