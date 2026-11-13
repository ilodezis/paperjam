CREATE TABLE document_organizations (
  document_id      TEXT NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
  correspondent_id TEXT NOT NULL REFERENCES correspondents (id) ON DELETE CASCADE,
  role             TEXT NOT NULL DEFAULT 'unspecified'
                   CHECK (role IN ('issued_by','received_from','party','employer','unspecified')),
  provenance       TEXT NOT NULL CHECK (provenance IN ('llm','mrz','manual','local')),
  PRIMARY KEY (document_id, correspondent_id, role)
);
CREATE INDEX document_organizations_correspondent ON document_organizations (correspondent_id);

INSERT INTO document_organizations (document_id, correspondent_id, role, provenance)
SELECT id, correspondent_id, 'unspecified', COALESCE(correspondent_provenance, 'local')
FROM documents
WHERE correspondent_id IS NOT NULL;

-- The old columns documents.correspondent_id and correspondent_provenance are
-- retired and always NULL. SQLite cannot drop a foreign-key column without a table
-- rebuild, so we set them to NULL and drop the index.
UPDATE documents SET correspondent_id = NULL, correspondent_provenance = NULL;

DROP INDEX documents_correspondent;
