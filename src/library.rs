//! The catalog persisted in SQLite (via sqlx).
//!
//! A book is a logical *work* (the `books` table) with one or more format files
//! (the `book_files` table): scanning an EPUB and an XTC of the same work — same
//! title and author, via `work_key` — attaches both to one book. The book's
//! metadata and cover come from the highest-`meta_rank` format present.
//!
//! Rather than holding every book in memory, the catalog is queried per request.
//! The store is also the shared, mutable state the file watcher updates as files
//! come and go, and connections come from a pool so reads proceed concurrently.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::catalog::{self, Acquisition, Book, BookFile, BookSource, Category, Format, UsdCents};
use crate::epub::{CoverRef, EpubMeta};

/// The `books` columns loaded into a [`BookRow`], in a fixed order.
const BOOK_COLUMNS: &str = "id, title, author, language, description, modified, \
     price_cents, lendable, cover_zip_path, cover_media_type";

/// A flat row from the `books` table; combined with its files to make a [`Book`].
#[derive(sqlx::FromRow)]
struct BookRow {
    id: String,
    title: String,
    author: String,
    language: Option<String>,
    description: Option<String>,
    modified: Option<String>,
    price_cents: Option<i64>,
    lendable: i64,
    cover_zip_path: Option<String>,
    cover_media_type: Option<String>,
}

impl BookRow {
    fn into_book(self, files: Vec<BookFile>) -> Book {
        let source = if files.is_empty() {
            BookSource::Sample
        } else {
            BookSource::Files(files)
        };
        let cover = match (self.cover_zip_path, self.cover_media_type) {
            (Some(zip_path), Some(media_type)) => Some(CoverRef {
                zip_path,
                media_type,
            }),
            _ => None,
        };
        // Exactly one acquisition mode applies; a borrow takes precedence over a
        // stray price, and no price means a free download.
        let acquisition = if self.lendable != 0 {
            Acquisition::Borrow
        } else if let Some(cents) = self.price_cents {
            Acquisition::Buy(UsdCents(cents.clamp(0, i64::from(u32::MAX)) as u32))
        } else {
            Acquisition::OpenAccess
        };
        Book {
            id: self.id,
            title: self.title,
            author: self.author,
            language: self.language,
            description: self.description,
            modified: self.modified.as_deref().and_then(catalog::parse_timestamp),
            acquisition,
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
        let row = sqlx::query_as!(
            BookRow,
            "SELECT id, title, author, language, description, modified,
                    price_cents, lendable, cover_zip_path, cover_media_type
             FROM books WHERE id = ?",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()?;
        let files = self.files_for(&row.id).await;
        Some(row.into_book(files))
    }

    /// A page of books ordered by title.
    pub async fn page(&self, limit: u64, offset: u64) -> Vec<Book> {
        let limit = limit as i64;
        let offset = offset as i64;
        let rows = sqlx::query_as!(
            BookRow,
            "SELECT id, title, author, language, description, modified,
                    price_cents, lendable, cover_zip_path, cover_media_type
             FROM books ORDER BY title COLLATE NOCASE LIMIT ? OFFSET ?",
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        self.hydrate(rows).await
    }

    /// The most recently modified books, newest first.
    pub async fn recent(&self, limit: u64) -> Vec<Book> {
        let limit = limit as i64;
        let rows = sqlx::query_as!(
            BookRow,
            "SELECT id, title, author, language, description, modified,
                    price_cents, lendable, cover_zip_path, cover_media_type
             FROM books ORDER BY modified DESC LIMIT ?",
            limit
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        self.hydrate(rows).await
    }

    /// All books, ordered by title (used by tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn all(&self) -> Vec<Book> {
        let rows = sqlx::query_as!(
            BookRow,
            "SELECT id, title, author, language, description, modified,
                    price_cents, lendable, cover_zip_path, cover_media_type
             FROM books ORDER BY title COLLATE NOCASE"
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        self.hydrate(rows).await
    }

    /// Search books. `query` matches title/author/description; `author` and
    /// `title` constrain those fields. All supplied (already-lowercased) terms
    /// must match. With no terms, returns nothing.
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
            "SELECT {BOOK_COLUMNS} FROM books WHERE {} ORDER BY title COLLATE NOCASE",
            clauses.join(" AND ")
        );
        let mut query = sqlx::query_as::<_, BookRow>(&sql);
        for bind in binds {
            query = query.bind(bind);
        }
        let rows = query.fetch_all(&self.pool).await.unwrap_or_default();
        self.hydrate(rows).await
    }

    /// Turn book rows into books by attaching each one's format files.
    async fn hydrate(&self, rows: Vec<BookRow>) -> Vec<Book> {
        let mut books = Vec::with_capacity(rows.len());
        for row in rows {
            let files = self.files_for(&row.id).await;
            books.push(row.into_book(files));
        }
        books
    }

    /// The format files for a book, best-metadata format first.
    async fn files_for(&self, book_id: &str) -> Vec<BookFile> {
        sqlx::query!(
            r#"SELECT path AS "path!", media_type AS "media_type!"
               FROM book_files WHERE book_id = ? ORDER BY media_type"#,
            book_id
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .filter_map(|r| match Format::from_media_type(&r.media_type) {
                    Some(format) => Some(BookFile {
                        path: PathBuf::from(r.path),
                        format,
                    }),
                    None => {
                        tracing::warn!(media_type = %r.media_type, "unknown stored media type");
                        None
                    }
                })
                .collect()
        })
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
        let rows = sqlx::query_as!(
            BookRow,
            r#"SELECT b.id AS "id!", b.title AS "title!", b.author AS "author!",
                      b.language, b.description, b.modified, b.price_cents,
                      b.lendable AS "lendable!", b.cover_zip_path, b.cover_media_type
               FROM books b
               JOIN book_categories bc ON bc.book_id = b.id
               WHERE bc.category_slug = ?
               ORDER BY b.title COLLATE NOCASE"#,
            slug
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        self.hydrate(rows).await
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
        let rows = sqlx::query_as!(
            BookRow,
            "SELECT id, title, author, language, description, modified,
                    price_cents, lendable, cover_zip_path, cover_media_type
             FROM books WHERE author = ? ORDER BY title COLLATE NOCASE",
            author
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        self.hydrate(rows).await
    }

    // --- Management (CLI / admin) ---

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

    /// Remove a book (and its files/category associations). Returns whether it
    /// existed.
    pub async fn remove_book(&self, id: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query!("DELETE FROM books WHERE id = ?", id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    // --- Reconciliation with the library directory ---

    /// Backfill `work_key` for books that predate it (e.g. after migration), so
    /// re-scanning their files groups onto the same book instead of duplicating.
    pub async fn backfill_work_keys(&self) {
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", title AS "title!", author AS "author!"
               FROM books WHERE work_key IS NULL"#
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        for row in rows {
            let key = catalog::work_key(&row.title, &row.author);
            let _ = sqlx::query!("UPDATE books SET work_key = ? WHERE id = ?", key, row.id)
                .execute(&self.pool)
                .await;
        }
    }

    /// Reconcile the catalog with the current contents of `dir`: unchanged files
    /// (matching stored mtime) are skipped, changed/new files are (re)read and
    /// ingested, files that no longer exist are removed, and books left with no
    /// files are deleted.
    pub async fn reconcile_dir(&self, dir: &Path) {
        let paths = catalog::book_file_paths(dir);
        let mut seen: HashSet<PathBuf> = HashSet::new();

        for path in &paths {
            seen.insert(path.clone());
            let mtime = file_mtime(path);
            if mtime.is_some() && self.stored_file_mtime(path).await == mtime {
                continue; // unchanged since last scan
            }
            let Some(format) = Format::from_path(path) else {
                continue; // not a supported book file (book_file_paths already filters)
            };
            match format.read_meta(path) {
                Ok(meta) => self.ingest_file(dir, path, format, meta, mtime).await,
                Err(err) => {
                    tracing::warn!(?err, path = %path.display(), "skipping unreadable book file");
                }
            }
        }

        self.delete_missing_files(&seen).await;
        self.delete_orphan_books().await;
        let count = self.count().await;
        tracing::info!(count, dir = %dir.display(), "catalog reconciled");
    }

    /// Read and ingest a single file (used by the watcher for a create/modify).
    pub async fn ingest(&self, dir: &Path, path: &Path) {
        let mtime = file_mtime(path);
        let Some(format) = Format::from_path(path) else {
            tracing::warn!(path = %path.display(), "ignoring unsupported file");
            return;
        };
        match format.read_meta(path) {
            Ok(meta) => self.ingest_file(dir, path, format, meta, mtime).await,
            Err(err) => {
                tracing::warn!(?err, path = %path.display(), "dropping unreadable book file");
                self.delete_by_path(path).await;
            }
        }
    }

    /// Attach a file to its work (creating the book if new), applying metadata
    /// from the richer format.
    async fn ingest_file(
        &self,
        dir: &Path,
        path: &Path,
        format: Format,
        meta: EpubMeta,
        mtime: Option<i64>,
    ) {
        let path_str = path.to_string_lossy().into_owned();
        let media_type = format.media_type();
        let rank = format.rank();

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("book");
        let title = meta.title.clone().unwrap_or_else(|| stem.to_string());
        let author = meta
            .author
            .clone()
            .unwrap_or_else(|| "Unknown Author".to_string());
        let work = catalog::work_key(&title, &author);

        let existing = sqlx::query!(
            r#"SELECT id AS "id!", meta_rank AS "meta_rank!: i64" FROM books WHERE work_key = ?"#,
            work
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();

        let book_id = match existing {
            Some(record) => {
                if rank > record.meta_rank {
                    self.write_metadata(&record.id, &meta, &title, &author, rank).await;
                }
                record.id
            }
            None => {
                let id = self.free_id(&catalog::slugify(&title)).await;
                self.create_book(&id, &work, &meta, &title, &author, rank).await;
                let category = catalog::derive_category(dir, path, &meta.subjects);
                if let Err(err) = self.seed_category(&id, &category).await {
                    tracing::error!(?err, id, "failed to seed book category");
                }
                id
            }
        };

        let _ = sqlx::query!(
            "INSERT INTO book_files (path, book_id, media_type, file_mtime)
                 VALUES (?, ?, ?, ?)
             ON CONFLICT(path) DO UPDATE SET
                 book_id = excluded.book_id,
                 media_type = excluded.media_type,
                 file_mtime = excluded.file_mtime",
            path_str,
            book_id,
            media_type,
            mtime,
        )
        .execute(&self.pool)
        .await;
    }

    /// Insert a new book (work) from a format's metadata.
    async fn create_book(
        &self,
        id: &str,
        work: &str,
        meta: &EpubMeta,
        title: &str,
        author: &str,
        rank: i64,
    ) {
        let (cover_zip, cover_type) = meta_cover_columns(meta);
        let language = meta.language.as_deref();
        let description = meta.description.as_deref();
        let modified = normalize_modified(meta);
        if let Err(err) = sqlx::query!(
            "INSERT INTO books (id, work_key, title, author, language, description,
                 modified, cover_zip_path, cover_media_type, meta_rank)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            id,
            work,
            title,
            author,
            language,
            description,
            modified,
            cover_zip,
            cover_type,
            rank,
        )
        .execute(&self.pool)
        .await
        {
            tracing::error!(?err, id, "failed to create book");
        }
    }

    /// Replace a book's metadata (a richer format was found).
    async fn write_metadata(
        &self,
        id: &str,
        meta: &EpubMeta,
        title: &str,
        author: &str,
        rank: i64,
    ) {
        let (cover_zip, cover_type) = meta_cover_columns(meta);
        let language = meta.language.as_deref();
        let description = meta.description.as_deref();
        let modified = normalize_modified(meta);
        let _ = sqlx::query!(
            "UPDATE books SET title = ?, author = ?, language = ?, description = ?,
                 modified = ?, cover_zip_path = ?, cover_media_type = ?, meta_rank = ?
             WHERE id = ?",
            title,
            author,
            language,
            description,
            modified,
            cover_zip,
            cover_type,
            rank,
            id,
        )
        .execute(&self.pool)
        .await;
    }

    /// Delete the file at a specific path and any book left with no files.
    pub async fn delete_by_path(&self, path: &Path) {
        let path = path.to_string_lossy();
        let _ = sqlx::query!("DELETE FROM book_files WHERE path = ?", path)
            .execute(&self.pool)
            .await;
        self.delete_orphan_books().await;
    }

    /// Whether any file is stored under the directory `dir` (used by the watcher
    /// to decide whether a removed directory affected the catalog).
    pub async fn has_books_under(&self, dir: &Path) -> bool {
        let mut prefix = dir.to_string_lossy().into_owned();
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        prefix.push('%');
        sqlx::query_scalar!(
            r#"SELECT path AS "path!" FROM book_files WHERE path LIKE ? LIMIT 1"#,
            prefix
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .is_some()
    }

    async fn stored_file_mtime(&self, path: &Path) -> Option<i64> {
        let path = path.to_string_lossy();
        sqlx::query_scalar!("SELECT file_mtime FROM book_files WHERE path = ?", path)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .flatten()
    }

    async fn delete_missing_files(&self, seen: &HashSet<PathBuf>) {
        let existing: Vec<String> =
            sqlx::query_scalar!(r#"SELECT path AS "path!" FROM book_files"#)
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        for path in existing {
            if !seen.contains(Path::new(&path)) {
                let _ = sqlx::query!("DELETE FROM book_files WHERE path = ?", path)
                    .execute(&self.pool)
                    .await;
            }
        }
    }

    /// Remove books that have no format files (their formats are all gone).
    async fn delete_orphan_books(&self) {
        let _ = sqlx::query!("DELETE FROM books WHERE id NOT IN (SELECT book_id FROM book_files)")
            .execute(&self.pool)
            .await;
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

    /// Replace the whole catalog with the built-in sample set (test-only).
    #[cfg(test)]
    pub async fn reset_to_samples(&self) {
        if let Err(err) = sqlx::query!("DELETE FROM books").execute(&self.pool).await {
            tracing::error!(?err, "failed to clear catalog");
            return;
        }
        for (book, category) in catalog::sample_books() {
            let modified = book.modified.map(|t| t.to_string());
            let (price_cents, lendable): (Option<i64>, i64) = match book.acquisition {
                Acquisition::OpenAccess => (None, 0),
                Acquisition::Buy(cents) => (Some(i64::from(cents.0)), 0),
                Acquisition::Borrow => (None, 1),
            };
            let work = catalog::work_key(&book.title, &book.author);
            if let Err(err) = sqlx::query!(
                "INSERT INTO books (id, work_key, title, author, language, description,
                     modified, price_cents, lendable, meta_rank)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
                book.id,
                work,
                book.title,
                book.author,
                book.language,
                book.description,
                modified,
                price_cents,
                lendable,
            )
            .execute(&self.pool)
            .await
            {
                tracing::error!(?err, id = %book.id, "failed to seed sample book");
                continue;
            }
            if let Err(err) = self.seed_category(&book.id, &category).await {
                tracing::error!(?err, id = %book.id, "failed to seed sample category");
            }
        }
    }
}

/// The cover columns `(zip_path, media_type)` for a format's metadata.
fn meta_cover_columns(meta: &EpubMeta) -> (Option<String>, Option<String>) {
    match &meta.cover {
        Some(cover) => (Some(cover.zip_path.clone()), Some(cover.media_type.clone())),
        None => (None, None),
    }
}

/// A format's `modified` field, normalized to an RFC 3339 string (or `None`).
fn normalize_modified(meta: &EpubMeta) -> Option<String> {
    meta.modified
        .as_deref()
        .and_then(catalog::parse_timestamp)
        .map(|t| t.to_string())
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
