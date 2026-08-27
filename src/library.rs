//! The catalog persisted in SQLite (via sqlx).
//!
//! Rather than holding every book in memory, the catalog lives in a `books`
//! table and handlers query it per request. The store is also the shared,
//! mutable state the file watcher updates as EPUBs come and go. Connections come
//! from a pool, so reads can proceed concurrently.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::catalog::{self, Book, BookSource, Category};
use crate::db;
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
    category: String,
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
            modified: self.modified,
            category: Category::from_slug(&self.category).unwrap_or(Category::NonFiction),
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
    /// Open (creating if needed) the catalog database at `path`.
    pub async fn open(path: &Path) -> Result<Self, sqlx::Error> {
        Ok(CatalogStore {
            pool: db::connect(path).await?,
        })
    }

    /// An ephemeral in-memory catalog (used by tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn open_in_memory() -> Result<Self, sqlx::Error> {
        Ok(CatalogStore {
            pool: db::connect_memory().await?,
        })
    }

    // --- Queries used by request handlers ---

    /// Total number of books.
    pub async fn count(&self) -> u64 {
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!: i64" FROM books"#)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0) as u64
    }

    /// Number of books in a category.
    pub async fn count_category(&self, category: Category) -> u64 {
        let slug = category.slug();
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!: i64" FROM books WHERE category = ?"#,
            slug
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0) as u64
    }

    /// Look up a single book by id.
    pub async fn get(&self, id: &str) -> Option<Book> {
        sqlx::query_as!(
            BookRow,
            "SELECT id, file_path, title, author, language, description, modified,
                    category, price_usd, lendable, cover_zip_path, cover_media_type
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
                    category, price_usd, lendable, cover_zip_path, cover_media_type
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
                    category, price_usd, lendable, cover_zip_path, cover_media_type
             FROM books ORDER BY modified DESC LIMIT ?",
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map(into_books)
        .unwrap_or_default()
    }

    /// All books in a category, ordered by title.
    pub async fn by_category(&self, category: Category) -> Vec<Book> {
        let slug = category.slug();
        sqlx::query_as!(
            BookRow,
            "SELECT id, file_path, title, author, language, description, modified,
                    category, price_usd, lendable, cover_zip_path, cover_media_type
             FROM books WHERE category = ? ORDER BY title COLLATE NOCASE",
            slug
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
                    category, price_usd, lendable, cover_zip_path, cover_media_type
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
                    category, price_usd, lendable, cover_zip_path, cover_media_type
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

    // --- Mutations used at startup and by the watcher ---

    /// Replace the whole catalog with the built-in sample set.
    pub async fn reset_to_samples(&self) {
        if let Err(err) = sqlx::query!("DELETE FROM books").execute(&self.pool).await {
            tracing::error!(%err, "failed to clear catalog");
            return;
        }
        for book in catalog::sample_books() {
            if let Err(err) = self.insert(&book.id, &book, None).await {
                tracing::error!(%err, id = %book.id, "failed to seed sample book");
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
                    let book = catalog::book_from_file(dir, path.clone(), meta);
                    self.upsert_file(&book, mtime).await;
                }
                Err(err) => {
                    tracing::warn!(%err, path = %path.display(), "skipping unreadable EPUB");
                }
            }
        }

        self.delete_missing(&seen).await;
        tracing::info!(count = self.count().await, dir = %dir.display(), "catalog reconciled");
    }

    /// Insert or update a single file-backed book, keeping its id stable across
    /// metadata changes.
    pub async fn upsert_file(&self, book: &Book, mtime: Option<i64>) {
        let BookSource::File { path } = &book.source else {
            return;
        };
        let path = path.to_string_lossy().into_owned();

        let existing: Option<String> =
            sqlx::query_scalar!("SELECT id FROM books WHERE file_path = ?", path)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();

        let result: Result<(), sqlx::Error> = if existing.is_some() {
            let (cover_zip, cover_type) = cover_columns(book);
            let slug = book.category.slug();
            let lendable = book.lendable as i64;
            sqlx::query!(
                "UPDATE books SET file_mtime = ?, title = ?, author = ?, language = ?,
                     description = ?, modified = ?, category = ?, price_usd = ?,
                     lendable = ?, cover_zip_path = ?, cover_media_type = ?
                 WHERE file_path = ?",
                mtime,
                book.title,
                book.author,
                book.language,
                book.description,
                book.modified,
                slug,
                book.price_usd,
                lendable,
                cover_zip,
                cover_type,
                path,
            )
            .execute(&self.pool)
            .await
            .map(|_| ())
        } else {
            let id = self.free_id(&book.id).await;
            self.insert(&id, book, mtime).await
        };
        if let Err(err) = result {
            tracing::error!(%err, path, "failed to store book");
        }
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

    /// Insert a book row with the given id and mtime.
    async fn insert(&self, id: &str, book: &Book, mtime: Option<i64>) -> Result<(), sqlx::Error> {
        let file_path = match &book.source {
            BookSource::File { path } => Some(path.to_string_lossy().into_owned()),
            BookSource::Sample => None,
        };
        let (cover_zip, cover_type) = cover_columns(book);
        let slug = book.category.slug();
        let lendable = book.lendable as i64;
        sqlx::query!(
            "INSERT INTO books (id, file_path, file_mtime, title, author, language,
                 description, modified, category, price_usd, lendable,
                 cover_zip_path, cover_media_type)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            id,
            file_path,
            mtime,
            book.title,
            book.author,
            book.language,
            book.description,
            book.modified,
            slug,
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
