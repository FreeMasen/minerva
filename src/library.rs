//! The catalog persisted in SQLite.
//!
//! Rather than holding every book in memory, the catalog lives in a `books`
//! table and handlers query it per request. The store is also the shared,
//! mutable state the file watcher updates as EPUBs come and go.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog::{self, Book, BookSource, Category};
use crate::epub::CoverRef;

/// The columns selected when reconstructing a [`Book`], in a fixed order.
const SELECT: &str = "SELECT id, file_path, title, author, language, description, \
     modified, category, price_usd, lendable, cover_zip_path, cover_media_type FROM books";

/// A SQLite-backed catalog of books.
pub struct CatalogStore {
    conn: Mutex<Connection>,
}

impl CatalogStore {
    /// Open (creating if needed) the catalog database at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    /// An ephemeral in-memory catalog (used by tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS books (
                 id               TEXT PRIMARY KEY,
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
             CREATE INDEX IF NOT EXISTS idx_books_category ON books(category);",
        )?;
        Ok(CatalogStore {
            conn: Mutex::new(conn),
        })
    }

    // --- Queries used by request handlers ---

    /// Total number of books.
    pub fn count(&self) -> u64 {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM books", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    }

    /// Number of books in a category.
    pub fn count_category(&self, category: Category) -> u64 {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM books WHERE category = ?1",
                [category.slug()],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64
    }

    /// Look up a single book by id.
    pub fn get(&self, id: &str) -> Option<Book> {
        self.query_books(&format!("{SELECT} WHERE id = ?1"), [id])
            .into_iter()
            .next()
    }

    /// A page of books ordered by title.
    pub fn page(&self, limit: u64, offset: u64) -> Vec<Book> {
        self.query_books(
            &format!("{SELECT} ORDER BY title COLLATE NOCASE LIMIT ?1 OFFSET ?2"),
            params![limit as i64, offset as i64],
        )
    }

    /// The most recently modified books, newest first.
    pub fn recent(&self, limit: u64) -> Vec<Book> {
        self.query_books(
            &format!("{SELECT} ORDER BY modified DESC LIMIT ?1"),
            params![limit as i64],
        )
    }

    /// All books in a category, ordered by title.
    pub fn by_category(&self, category: Category) -> Vec<Book> {
        self.query_books(
            &format!("{SELECT} WHERE category = ?1 ORDER BY title COLLATE NOCASE"),
            [category.slug()],
        )
    }

    /// All books, ordered by title (used by tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn all(&self) -> Vec<Book> {
        self.query_books(&format!("{SELECT} ORDER BY title COLLATE NOCASE"), [])
    }

    /// Search books. `query` matches title/author/description; `author` and
    /// `title` constrain those fields. All supplied (already-lowercased) terms
    /// must match. With no terms, returns nothing.
    pub fn search(&self, query: &str, author: &str, title: &str) -> Vec<Book> {
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
            "{SELECT} WHERE {} ORDER BY title COLLATE NOCASE",
            clauses.join(" AND ")
        );
        self.query_books(&sql, rusqlite::params_from_iter(binds))
    }

    fn query_books(&self, sql: &str, params: impl rusqlite::Params) -> Vec<Book> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(sql) {
            Ok(stmt) => stmt,
            Err(err) => {
                tracing::error!(%err, "failed to prepare catalog query");
                return Vec::new();
            }
        };
        match stmt.query_map(params, row_to_book) {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(err) => {
                tracing::error!(%err, "failed to run catalog query");
                Vec::new()
            }
        }
    }

    // --- Mutations used at startup and by the watcher ---

    /// Replace the whole catalog with the built-in sample set.
    pub fn reset_to_samples(&self) {
        let conn = self.conn.lock().unwrap();
        if let Err(err) = conn.execute("DELETE FROM books", []) {
            tracing::error!(%err, "failed to clear catalog");
            return;
        }
        for book in catalog::sample_books() {
            if let Err(err) = insert_row(&conn, &book, None) {
                tracing::error!(%err, id = %book.id, "failed to seed sample book");
            }
        }
    }

    /// Remove sample (non-file) books.
    pub fn remove_sample_books(&self) {
        let _ = self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM books WHERE file_path IS NULL", []);
    }

    /// Reconcile the catalog with the current contents of `dir`: unchanged files
    /// (matching stored mtime) are skipped, changed/new files are (re)read, and
    /// rows for files that no longer exist are removed.
    pub fn reconcile_dir(&self, dir: &Path) {
        let paths = catalog::epub_paths(dir);
        let mut seen: HashSet<PathBuf> = HashSet::new();

        for path in &paths {
            seen.insert(path.clone());
            let mtime = file_mtime(path);
            if mtime.is_some() && self.stored_mtime(path) == mtime {
                continue; // unchanged since last scan
            }
            match crate::epub::read_meta(path) {
                Ok(meta) => {
                    let book = catalog::book_from_file(dir, path.clone(), meta);
                    self.upsert_file(&book, mtime);
                }
                Err(err) => {
                    tracing::warn!(%err, path = %path.display(), "skipping unreadable EPUB");
                }
            }
        }

        self.delete_missing(&seen);
        tracing::info!(count = self.count(), dir = %dir.display(), "catalog reconciled");
    }

    /// Insert or update a single file-backed book, keeping its id stable across
    /// metadata changes.
    pub fn upsert_file(&self, book: &Book, mtime: Option<i64>) {
        let BookSource::File { path } = &book.source else {
            return;
        };
        let path = path.to_string_lossy().into_owned();
        let (cover_zip, cover_type) = cover_columns(book);

        let conn = self.conn.lock().unwrap();
        let existing: Option<String> = conn
            .query_row("SELECT id FROM books WHERE file_path = ?1", [&path], |r| {
                r.get(0)
            })
            .optional()
            .unwrap_or(None);

        let result = if existing.is_some() {
            conn.execute(
                "UPDATE books SET file_mtime = ?1, title = ?2, author = ?3, language = ?4,
                     description = ?5, modified = ?6, category = ?7, price_usd = ?8,
                     lendable = ?9, cover_zip_path = ?10, cover_media_type = ?11
                 WHERE file_path = ?12",
                params![
                    mtime,
                    book.title,
                    book.author,
                    book.language,
                    book.description,
                    book.modified,
                    book.category.slug(),
                    book.price_usd,
                    book.lendable as i64,
                    cover_zip,
                    cover_type,
                    path,
                ],
            )
        } else {
            let id = free_id(&conn, &book.id);
            conn.execute(
                "INSERT INTO books (id, file_path, file_mtime, title, author, language,
                     description, modified, category, price_usd, lendable,
                     cover_zip_path, cover_media_type)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    id,
                    path,
                    mtime,
                    book.title,
                    book.author,
                    book.language,
                    book.description,
                    book.modified,
                    book.category.slug(),
                    book.price_usd,
                    book.lendable as i64,
                    cover_zip,
                    cover_type,
                ],
            )
        };
        if let Err(err) = result {
            tracing::error!(%err, path, "failed to store book");
        }
    }

    /// Delete the book backed by a specific file path, if any.
    pub fn delete_by_path(&self, path: &Path) {
        let _ = self.conn.lock().unwrap().execute(
            "DELETE FROM books WHERE file_path = ?1",
            [path.to_string_lossy().as_ref()],
        );
    }

    /// Whether any book is stored under the directory `dir` (used by the watcher
    /// to decide whether a removed directory affected the catalog).
    pub fn has_books_under(&self, dir: &Path) -> bool {
        let mut prefix = dir.to_string_lossy().into_owned();
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        prefix.push('%');
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT 1 FROM books WHERE file_path LIKE ?1 LIMIT 1",
                [prefix],
                |_| Ok(()),
            )
            .optional()
            .unwrap_or(None)
            .is_some()
    }

    fn stored_mtime(&self, path: &Path) -> Option<i64> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT file_mtime FROM books WHERE file_path = ?1",
                [path.to_string_lossy().as_ref()],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
    }

    fn delete_missing(&self, seen: &HashSet<PathBuf>) {
        let conn = self.conn.lock().unwrap();
        let existing: Vec<String> = {
            let mut stmt = match conn.prepare("SELECT file_path FROM books WHERE file_path IS NOT NULL")
            {
                Ok(stmt) => stmt,
                Err(err) => {
                    tracing::error!(%err, "failed to list stored paths");
                    return;
                }
            };
            let rows = stmt.query_map([], |r| r.get::<_, String>(0));
            match rows {
                Ok(rows) => rows.filter_map(Result::ok).collect(),
                Err(_) => return,
            }
        };
        for path in existing {
            if !seen.contains(Path::new(&path)) {
                let _ = conn.execute("DELETE FROM books WHERE file_path = ?1", [&path]);
            }
        }
    }
}

/// Reconstruct a [`Book`] from a `books` row (columns as in [`SELECT`]).
fn row_to_book(row: &rusqlite::Row) -> rusqlite::Result<Book> {
    let file_path: Option<String> = row.get("file_path")?;
    let source = match file_path {
        Some(path) => BookSource::File {
            path: PathBuf::from(path),
        },
        None => BookSource::Sample,
    };

    let cover_zip: Option<String> = row.get("cover_zip_path")?;
    let cover_type: Option<String> = row.get("cover_media_type")?;
    let cover = match (cover_zip, cover_type) {
        (Some(zip_path), Some(media_type)) => Some(CoverRef {
            zip_path,
            media_type,
        }),
        _ => None,
    };

    let category_slug: String = row.get("category")?;
    let category = Category::from_slug(&category_slug).unwrap_or(Category::NonFiction);

    Ok(Book {
        id: row.get("id")?,
        title: row.get("title")?,
        author: row.get("author")?,
        language: row.get("language")?,
        description: row.get("description")?,
        modified: row.get("modified")?,
        category,
        price_usd: row.get("price_usd")?,
        lendable: row.get::<_, i64>("lendable")? != 0,
        source,
        cover,
    })
}

/// The cover columns for a book: `(zip_path, media_type)`.
fn cover_columns(book: &Book) -> (Option<String>, Option<String>) {
    match &book.cover {
        Some(cover) => (Some(cover.zip_path.clone()), Some(cover.media_type.clone())),
        None => (None, None),
    }
}

/// Insert a book row with a caller-provided mtime (used for sample seeding).
fn insert_row(conn: &Connection, book: &Book, mtime: Option<i64>) -> rusqlite::Result<()> {
    let file_path = match &book.source {
        BookSource::File { path } => Some(path.to_string_lossy().into_owned()),
        BookSource::Sample => None,
    };
    let (cover_zip, cover_type) = cover_columns(book);
    conn.execute(
        "INSERT INTO books (id, file_path, file_mtime, title, author, language,
             description, modified, category, price_usd, lendable,
             cover_zip_path, cover_media_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            book.id,
            file_path,
            mtime,
            book.title,
            book.author,
            book.language,
            book.description,
            book.modified,
            book.category.slug(),
            book.price_usd,
            book.lendable as i64,
            cover_zip,
            cover_type,
        ],
    )?;
    Ok(())
}

/// Find an unused id, appending `-2`, `-3`, … to `base` on collision.
fn free_id(conn: &Connection, base: &str) -> String {
    let taken = |id: &str| {
        conn.query_row("SELECT 1 FROM books WHERE id = ?1", [id], |_| Ok(()))
            .optional()
            .unwrap_or(None)
            .is_some()
    };
    if !taken(base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
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
