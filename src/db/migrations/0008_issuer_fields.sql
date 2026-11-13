-- A field definition can say it names the document's issuer (`"issuer": true`):
-- the model's answer for it can then link the issuing organization when
-- classification found none. Before the flag the pipeline knew a fixed list of
-- field keys; those keys, on text fields, get the flag so types stored earlier
-- (built-in or user-made) keep their behavior. Anything else is left alone.
UPDATE document_types
SET fields = (
    SELECT json_group_array(
        CASE WHEN json_extract(value, '$.kind') = 'text'
              AND json_extract(value, '$.key') IN (
                  'issuing_authority', 'registry_office', 'issuer',
                  'tax_authority', 'institution', 'insurer', 'bank')
             THEN json_set(value, '$.issuer', json('true'))
             ELSE json(value)
        END)
    FROM (SELECT value FROM json_each(document_types.fields) ORDER BY key)
)
WHERE json_valid(fields);
