-- Identifiers found for a book: in its OPF metadata, printed in its text, or
-- listed in the Library of Congress CIP record it reproduces.
--
-- A row per (kind, value) rather than a column on `books`, because a book
-- legitimately has several: a copyright page commonly prints a hardcover and
-- an ebook ISBN, and either may be the one an external catalog knows. `source`
-- records where each came from, so a disagreement between the OPF and the
-- text stays visible instead of one silently overwriting the other.
CREATE TABLE book_identifiers (
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    kind    TEXT NOT NULL,          -- 'isbn' (normalized to ISBN-13) or 'lccn'
    value   TEXT NOT NULL,
    source  TEXT NOT NULL,          -- 'opf', 'content' or 'cip'
    PRIMARY KEY (book_id, kind, value, source)
);

-- Looking a book up by an identifier is the point of storing them.
CREATE INDEX idx_book_identifiers_value ON book_identifiers(kind, value);
