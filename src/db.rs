//! SQLite connection-pool setup shared by the catalog and user stores.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

/// Open a pooled connection to the database at `path`, creating the file if
/// needed and applying migrations.
///
/// WAL mode plus a busy timeout let readers (request handlers) and the single
/// writer (the file watcher) proceed concurrently without spurious
/// `SQLITE_BUSY` errors.
pub async fn connect(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    migrated_pool(options, 5).await
}

/// Open a private in-memory database (used by tests). A single connection keeps
/// the database alive for the pool's lifetime.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn connect_memory() -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    migrated_pool(options, 1).await
}

async fn migrated_pool(
    options: SqliteConnectOptions,
    max_connections: u32,
) -> Result<SqlitePool, sqlx::Error> {
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}
