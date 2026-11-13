-- A fact's label in the language of the user who uploaded the document
-- ("Снят с регистрационного учёта"), written by the model next to the stable
-- snake_case key. NULL for facts from before and for facts without one: the
-- UI then falls back to a translation of the key.

ALTER TABLE facts ADD COLUMN label TEXT;
