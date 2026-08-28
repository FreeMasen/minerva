-- Move categories out of a fixed column on `books` into their own tables,
-- allowing arbitrary, create-on-demand categories with a many-to-many mapping.

DROP INDEX IF EXISTS idx_books_category;
ALTER TABLE books DROP COLUMN category;

CREATE TABLE categories (
    slug  TEXT PRIMARY KEY NOT NULL,
    label TEXT NOT NULL
);

CREATE TABLE book_categories (
    book_id       TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    category_slug TEXT NOT NULL REFERENCES categories(slug) ON DELETE CASCADE,
    PRIMARY KEY (book_id, category_slug)
);

CREATE INDEX idx_book_categories_slug ON book_categories(category_slug);
