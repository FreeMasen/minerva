-- A book becomes a logical work that can have several format files (EPUB, XTC,
-- ...). Files move into book_files; books gains a work_key (grouping by
-- title+author) and meta_rank (which format supplied the current metadata).
--
-- The old books.file_path / file_mtime columns are left in place but unused:
-- file_path carries a UNIQUE constraint, which SQLite won't let us DROP without
-- a full table rebuild. New rows leave them NULL.

CREATE TABLE book_files (
    path       TEXT PRIMARY KEY NOT NULL,
    book_id    TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    media_type TEXT NOT NULL,
    file_mtime INTEGER
);

CREATE INDEX idx_book_files_book ON book_files(book_id);

INSERT INTO book_files (path, book_id, media_type, file_mtime)
    SELECT file_path, id, 'application/epub+zip', file_mtime
    FROM books
    WHERE file_path IS NOT NULL;

ALTER TABLE books ADD COLUMN work_key TEXT;
ALTER TABLE books ADD COLUMN meta_rank INTEGER NOT NULL DEFAULT 0;

CREATE UNIQUE INDEX idx_books_work_key ON books(work_key);
