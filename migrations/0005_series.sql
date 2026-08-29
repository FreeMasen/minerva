-- A book may belong to a series, with a (possibly fractional) position in it.
-- Populated from EPUB metadata (Calibre's calibre:series / EPUB3
-- belongs-to-collection) and editable in the admin UI.

ALTER TABLE books ADD COLUMN series TEXT;
ALTER TABLE books ADD COLUMN series_index REAL;
