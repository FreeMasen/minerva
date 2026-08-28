//! SQLite connection-pool setup shared by the catalog and user stores.

use std::path::Path;
use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// Open a pooled connection to the database at `path`, creating the file if
/// needed and applying migrations.
pub async fn connect(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true);
    migrated_pool(options, 5).await
}

/// Open a private in-memory database (used by tests). A single connection keeps
/// the database alive for the pool's lifetime.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn connect_memory() -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
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
