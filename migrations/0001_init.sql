CREATE TABLE IF NOT EXISTS books (
    id               TEXT PRIMARY KEY NOT NULL,
    file_path        TEXT UNIQUE,
    file_mtime       INTEGER,
    title            TEXT NOT NULL,
    author           TEXT NOT NULL,
    language         TEXT,
    description      TEXT,
    modified         TEXT,
    category         TEXT NOT NULL,
    price_usd        REAL,
    lendable         INTEGER NOT NULL DEFAULT 0,
    cover_zip_path   TEXT,
    cover_media_type TEXT
);

CREATE INDEX IF NOT EXISTS idx_books_title ON books(title COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_books_category ON books(category);

CREATE TABLE IF NOT EXISTS users (
    username      TEXT PRIMARY KEY NOT NULL,
    password_hash TEXT NOT NULL,
    display_name  TEXT
);
