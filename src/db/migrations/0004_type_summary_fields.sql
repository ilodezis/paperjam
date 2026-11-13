-- The fields a document type shows on a search hit, in display order, as a
-- JSON array of field keys. NULL: not set (the first fields of the type are
-- shown); the built-in types are filled in by the startup seeding.
ALTER TABLE document_types ADD COLUMN summary_fields TEXT;
