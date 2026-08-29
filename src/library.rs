//! The catalog persisted in SQLite (via sqlx).
//!
//! Rather than holding every book in memory, the catalog lives in a `books`
//! table and handlers query it per request. The store is also the shared,
//! mutable state the file watcher updates as EPUBs come and go. Connections come
//! from a pool, so reads can proceed concurrently.
//!
//! Categories are arbitrary, created on demand, and joined to books many-to-many
//! via the `categories` and `book_categories` tables.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::catalog::{self, Book, BookSource, Category};
use crate::epub::CoverRef;

/// A flat row from the `books` table, convertible to a [`Book`].
#[derive(sqlx::FromRow)]
struct BookRow {
    id: String,
    file_path: Option<String>,
    title: String,
    author: String,
    language: Option<String>,
    description: Option<String>,
    modified: Option<String>,
    price_usd: Option<f64>,
    lendable: i64,
    cover_zip_path: Option<String>,
    cover_media_type: Option<String>,
}

impl BookRow {
    fn into_book(self) -> Book {
        let source = match self.file_path {
            Some(path) => BookSource::File {
                path: PathBuf::from(path),
            },
            None => BookSource::Sample,
        };
        let cover = match (self.cover_zip_path, self.cover_media_type) {
            (Some(zip_path), Some(media_type)) => Some(CoverRef {
                zip_path,
                media_type,
            }),
            _ => None,
        };
        Book {
            id: self.id,
            title: self.title,
            author: self.author,
            language: self.language,
            description: self.description,
            modified: self.modified.as_deref().and_then(catalog::parse_timestamp),
            price_usd: self.price_usd,
            lendable: self.lendable != 0,
            source,
            cover,
        }
    }
}

/// A SQLite-backed catalog of books.
pub struct CatalogStore {
    pool: SqlitePool,
}

impl CatalogStore {
    /// Wrap a shared connection pool.
    pub fn new(pool: SqlitePool) -> Self {
        CatalogStore { pool }
    }

    // --- Queries used by request handlers ---

    /// Total number of books.
    pub async fn count(&self) -> u64 {
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!: i64" FROM books"#)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0) as u64
    }

    /// Look up a single book by id.
    pub async fn get(&self, id: &str) -> Option<Book> {
        sqlx::query_as!(
            BookRow,
            "SELECT id, file_path, title, author, language, description, modified,
                    price_usd, lendable, cover_zip_path, cover_media_type
             FROM books WHERE id = ?",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .map(BookRow::into_book)
    }

    /// A page of books ordered by title.
    pub async fn page(&self, limit: u64, offset: u64) -> Vec<Book> {
        let limit = limit as i64;
        let offset = offset as i64;
        sqlx::query_as!(
            BookRow,
            "SELECT id, file_path, title, author, language, description, modified,
                    price_usd, lendable, cover_zip_path, cover_media_type
             FROM books ORDER BY title COLLATE NOCASE LIMIT ? OFFSET ?",
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await
        .map(into_books)
        .unwrap_or_default()
    }

    /// The most recently modified books, newest first.
    pub async fn recent(&self, limit: u64) -> Vec<Book> {
        let limit = limit as i64;
        sqlx::query_as!(
            BookRow,
            "SELECT id, file_path, title, author, language, description, modified,
                    price_usd, lendable, cover_zip_path, cover_media_type
             FROM books ORDER BY modified DESC LIMIT ?",
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map(into_books)
        .unwrap_or_default()
    }

    /// All books, ordered by title (used by tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn all(&self) -> Vec<Book> {
        sqlx::query_as!(
            BookRow,
            "SELECT id, file_path, title, author, language, description, modified,
                    price_usd, lendable, cover_zip_path, cover_media_type
             FROM books ORDER BY title COLLATE NOCASE"
        )
        .fetch_all(&self.pool)
        .await
        .map(into_books)
        .unwrap_or_default()
    }

    /// Search books. `query` matches title/author/description; `author` and
    /// `title` constrain those fields. All supplied (already-lowercased) terms
    /// must match. With no terms, returns nothing.
    ///
    /// The `WHERE` clause is dynamic, so this uses the runtime query builder
    /// (not the compile-checked macro).
    pub async fn search(&self, query: &str, author: &str, title: &str) -> Vec<Book> {
        let mut clauses: Vec<&str> = Vec::new();
        let mut binds: Vec<String> = Vec::new();

        if !query.is_empty() {
            clauses.push(
                "(LOWER(title) LIKE ? OR LOWER(author) LIKE ? OR LOWER(IFNULL(description,'')) LIKE ?)",
            );
            let pattern = format!("%{query}%");
            binds.push(pattern.clone());
            binds.push(pattern.clone());
            binds.push(pattern);
        }
        if !author.is_empty() {
            clauses.push("LOWER(author) LIKE ?");
            binds.push(format!("%{author}%"));
        }
        if !title.is_empty() {
            clauses.push("LOWER(title) LIKE ?");
            binds.push(format!("%{title}%"));
        }

        if clauses.is_empty() {
            return Vec::new();
        }

        let sql = format!(
            "SELECT id, file_path, title, author, language, description, modified,
                    price_usd, lendable, cover_zip_path, cover_media_type
             FROM books WHERE {} ORDER BY title COLLATE NOCASE",
            clauses.join(" AND ")
        );
        let mut query = sqlx::query_as::<_, BookRow>(&sql);
        for bind in binds {
            query = query.bind(bind);
        }
        query
            .fetch_all(&self.pool)
            .await
            .map(into_books)
            .unwrap_or_default()
    }

    // --- Categories ---

    /// Non-empty categories with their book counts, ordered by label.
    pub async fn categories(&self) -> Vec<(Category, u64)> {
        sqlx::query!(
            r#"SELECT c.slug AS "slug!", c.label AS "label!", COUNT(bc.book_id) AS "count!: i64"
               FROM categories c
               JOIN book_categories bc ON bc.category_slug = c.slug
               GROUP BY c.slug, c.label
               ORDER BY c.label COLLATE NOCASE"#
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|r| (Category::new(r.slug, r.label), r.count as u64))
                .collect()
        })
        .unwrap_or_default()
    }

    /// Look up a category by slug.
    pub async fn category(&self, slug: &str) -> Option<Category> {
        sqlx::query!("SELECT slug, label FROM categories WHERE slug = ?", slug)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .map(|r| Category::new(r.slug, r.label))
    }

    /// All books in a category (by slug), ordered by title.
    pub async fn books_in_category(&self, slug: &str) -> Vec<Book> {
        sqlx::query_as!(
            BookRow,
            r#"SELECT b.id AS "id!", b.file_path, b.title AS "title!", b.author AS "author!",
                      b.language, b.description, b.modified, b.price_usd,
                      b.lendable AS "lendable!", b.cover_zip_path, b.cover_media_type
               FROM books b
               JOIN book_categories bc ON bc.book_id = b.id
               WHERE bc.category_slug = ?
               ORDER BY b.title COLLATE NOCASE"#,
            slug
        )
        .fetch_all(&self.pool)
        .await
        .map(into_books)
        .unwrap_or_default()
    }

    /// The categories a book belongs to, ordered by label.
    pub async fn book_categories(&self, book_id: &str) -> Vec<Category> {
        sqlx::query!(
            r#"SELECT c.slug AS "slug!", c.label AS "label!"
               FROM categories c
               JOIN book_categories bc ON bc.category_slug = c.slug
               WHERE bc.book_id = ?
               ORDER BY c.label COLLATE NOCASE"#,
            book_id
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|r| Category::new(r.slug, r.label))
                .collect()
        })
        .unwrap_or_default()
    }

    /// Assign a category (by human-readable name) to a book, creating the
    /// category on demand. Returns the stored category.
    pub async fn assign_category(&self, book_id: &str, name: &str) -> Result<Category, sqlx::Error> {
        let category = Category::new(catalog::slugify(name), name.trim());
        self.seed_category(book_id, &category).await?;
        // Reflect the stored label, which wins if the category already existed.
        Ok(self.category(&category.slug).await.unwrap_or(category))
    }

    /// Remove a category from a book (idempotent).
    pub async fn remove_category(&self, book_id: &str, slug: &str) {
        let _ = sqlx::query!(
            "DELETE FROM book_categories WHERE book_id = ? AND category_slug = ?",
            book_id,
            slug
        )
        .execute(&self.pool)
        .await;
    }

    // --- Authors (a category-like browse dimension derived from the author
    // column, so no per-book associations are stored) ---

    /// Distinct authors with a URL slug and book count, ordered by name.
    pub async fn authors(&self) -> Vec<(Category, u64)> {
        sqlx::query!(
            r#"SELECT author AS "author!", COUNT(*) AS "count!: i64"
               FROM books GROUP BY author ORDER BY author COLLATE NOCASE"#
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|r| (Category::new(catalog::slugify(&r.author), r.author), r.count as u64))
                .collect()
        })
        .unwrap_or_default()
    }

    /// The author name matching a slug, if any.
    pub async fn author_by_slug(&self, slug: &str) -> Option<String> {
        sqlx::query_scalar!(r#"SELECT DISTINCT author AS "author!" FROM books"#)
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .find(|author| catalog::slugify(author) == slug)
    }

    /// All books by an author, ordered by title.
    pub async fn books_by_author(&self, author: &str) -> Vec<Book> {
        sqlx::query_as!(
            BookRow,
            "SELECT id, file_path, title, author, language, description, modified,
                    price_usd, lendable, cover_zip_path, cover_media_type
             FROM books WHERE author = ? ORDER BY title COLLATE NOCASE",
            author
        )
        .fetch_all(&self.pool)
        .await
        .map(into_books)
        .unwrap_or_default()
    }

    /// Create the category if needed and associate it with the book.
    async fn seed_category(&self, book_id: &str, category: &Category) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT OR IGNORE INTO categories (slug, label) VALUES (?, ?)",
            category.slug,
            category.label
        )
        .execute(&self.pool)
        .await?;
        sqlx::query!(
            "INSERT OR IGNORE INTO book_categories (book_id, category_slug) VALUES (?, ?)",
            book_id,
            category.slug
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --- Mutations used at startup and by the watcher ---

    /// Replace the whole catalog with the built-in sample set (test-only).
    #[cfg(test)]
    pub async fn reset_to_samples(&self) {
        if let Err(err) = sqlx::query!("DELETE FROM books").execute(&self.pool).await {
            tracing::error!(?err, "failed to clear catalog");
            return;
        }
        for (book, category) in catalog::sample_books() {
            if let Err(err) = self.insert(&book.id, &book, None).await {
                tracing::error!(?err, id = %book.id, "failed to seed sample book");
                continue;
            }
            if let Err(err) = self.seed_category(&book.id, &category).await {
                tracing::error!(?err, id = %book.id, "failed to seed sample category");
            }
        }
    }

    /// Remove sample (non-file) books.
    pub async fn remove_sample_books(&self) {
        let _ = sqlx::query!("DELETE FROM books WHERE file_path IS NULL")
            .execute(&self.pool)
            .await;
    }

    /// Reconcile the catalog with the current contents of `dir`: unchanged files
    /// (matching stored mtime) are skipped, changed/new files are (re)read, and
    /// rows for files that no longer exist are removed.
    pub async fn reconcile_dir(&self, dir: &Path) {
        let paths = catalog::epub_paths(dir);
        let mut seen: HashSet<PathBuf> = HashSet::new();

        for path in &paths {
            seen.insert(path.clone());
            let mtime = file_mtime(path);
            if mtime.is_some() && self.stored_mtime(path).await == mtime {
                continue; // unchanged since last scan
            }
            match crate::epub::read_meta(path) {
                Ok(meta) => {
                    let (book, category) = catalog::book_from_file(dir, path.clone(), meta);
                    self.upsert_file(&book, mtime, &category).await;
                }
                Err(err) => {
                    tracing::warn!(?err, path = %path.display(), "skipping unreadable EPUB");
                }
            }
        }

        self.delete_missing(&seen).await;
        let count = self.count().await;
        tracing::info!(count, dir = %dir.display(), "catalog reconciled");
    }

    /// Insert or update a single file-backed book, keeping its id stable across
    /// metadata changes. A newly-inserted book is filed under `default_category`;
    /// updates leave a book's (possibly hand-edited) categories untouched.
    pub async fn upsert_file(&self, book: &Book, mtime: Option<i64>, default_category: &Category) {
        let BookSource::File { path } = &book.source else {
            return;
        };
        let path = path.to_string_lossy().into_owned();

        // Whether this file is already known decides id allocation and default-
        // category seeding. The write itself is an atomic upsert keyed on
        // file_path, so a stale answer here is harmless (at worst a default
        // category is re-seeded).
        let is_new = sqlx::query_scalar!("SELECT id FROM books WHERE file_path = ?", path)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .is_none();

        // On conflict the existing id is kept, so this id is only used for a
        // genuinely new row; free_id keeps it from colliding with another book.
        let id = self.free_id(&book.id).await;
        if let Err(err) = self.insert(&id, book, mtime).await {
            tracing::error!(?err, path, "failed to store book");
            return;
        }

        if is_new {
            if let Err(err) = self.seed_category(&id, default_category).await {
                tracing::error!(?err, path, "failed to seed book category");
            }
        }
    }

    /// Set a book's title. Returns whether a book with that id existed.
    pub async fn set_title(&self, id: &str, title: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query!("UPDATE books SET title = ? WHERE id = ?", title, id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Set a book's author. Returns whether a book with that id existed.
    pub async fn set_author(&self, id: &str, author: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query!("UPDATE books SET author = ? WHERE id = ?", author, id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Remove a book (and its category associations). Returns whether it existed.
    pub async fn remove_book(&self, id: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query!("DELETE FROM books WHERE id = ?", id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Delete the book backed by a specific file path, if any.
    pub async fn delete_by_path(&self, path: &Path) {
        let path = path.to_string_lossy();
        let _ = sqlx::query!("DELETE FROM books WHERE file_path = ?", path)
            .execute(&self.pool)
            .await;
    }

    /// Whether any book is stored under the directory `dir` (used by the watcher
    /// to decide whether a removed directory affected the catalog).
    pub async fn has_books_under(&self, dir: &Path) -> bool {
        let mut prefix = dir.to_string_lossy().into_owned();
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        prefix.push('%');
        sqlx::query_scalar!("SELECT id FROM books WHERE file_path LIKE ? LIMIT 1", prefix)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    /// Insert a book row, or (for a file-backed book whose path is already
    /// stored) update it in place, keeping its existing id. The conflict target
    /// is `file_path`; sample books (NULL file_path) never conflict.
    async fn insert(&self, id: &str, book: &Book, mtime: Option<i64>) -> Result<(), sqlx::Error> {
        let file_path = match &book.source {
            BookSource::File { path } => Some(path.to_string_lossy().into_owned()),
            BookSource::Sample => None,
        };
        let (cover_zip, cover_type) = cover_columns(book);
        let lendable = book.lendable as i64;
        let modified = book.modified.as_ref().map(jiff::Timestamp::to_string);
        sqlx::query!(
            "INSERT INTO books (id, file_path, file_mtime, title, author, language,
                 description, modified, price_usd, lendable,
                 cover_zip_path, cover_media_type)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(file_path) DO UPDATE SET
                 file_mtime = excluded.file_mtime,
                 title = excluded.title,
                 author = excluded.author,
                 language = excluded.language,
                 description = excluded.description,
                 modified = excluded.modified,
                 price_usd = excluded.price_usd,
                 lendable = excluded.lendable,
                 cover_zip_path = excluded.cover_zip_path,
                 cover_media_type = excluded.cover_media_type",
            id,
            file_path,
            mtime,
            book.title,
            book.author,
            book.language,
            book.description,
            modified,
            book.price_usd,
            lendable,
            cover_zip,
            cover_type,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn stored_mtime(&self, path: &Path) -> Option<i64> {
        let path = path.to_string_lossy();
        sqlx::query_scalar!("SELECT file_mtime FROM books WHERE file_path = ?", path)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .flatten()
    }

    async fn delete_missing(&self, seen: &HashSet<PathBuf>) {
        let existing: Vec<String> =
            sqlx::query_scalar!(r#"SELECT file_path AS "file_path!: String" FROM books WHERE file_path IS NOT NULL"#)
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        for path in existing {
            if !seen.contains(Path::new(&path)) {
                let _ = sqlx::query!("DELETE FROM books WHERE file_path = ?", path)
                    .execute(&self.pool)
                    .await;
            }
        }
    }

    /// Find an unused id, appending `-2`, `-3`, … to `base` on collision.
    async fn free_id(&self, base: &str) -> String {
        if !self.id_taken(base).await {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !self.id_taken(&candidate).await {
                return candidate;
            }
            n += 1;
        }
    }

    async fn id_taken(&self, id: &str) -> bool {
        sqlx::query_scalar!("SELECT id FROM books WHERE id = ? LIMIT 1", id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .is_some()
    }
}

fn into_books(rows: Vec<BookRow>) -> Vec<Book> {
    rows.into_iter().map(BookRow::into_book).collect()
}

/// The cover columns for a book: `(zip_path, media_type)`.
fn cover_columns(book: &Book) -> (Option<String>, Option<String>) {
    match &book.cover {
        Some(cover) => (Some(cover.zip_path.clone()), Some(cover.media_type.clone())),
        None => (None, None),
    }
}

/// A file's modification time as Unix seconds.
fn file_mtime(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}
