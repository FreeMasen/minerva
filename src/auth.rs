//! User accounts and password verification, backed by SQLite.
//!
//! Passwords are hashed with Argon2 (RustCrypto) and stored as PHC strings;
//! verification uses Argon2's constant-time comparison. Account management is
//! done out-of-band via the `adduser` CLI subcommand.

use std::path::Path;
use std::sync::Mutex;

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand_core::OsRng;
use rusqlite::{Connection, OptionalExtension, params};

/// A store of user accounts backed by a SQLite database.
pub struct AuthStore {
    conn: Mutex<Connection>,
}

impl AuthStore {
    /// Open (creating if needed) the user database at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    /// An ephemeral in-memory store (used by tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                 username      TEXT PRIMARY KEY,
                 password_hash TEXT NOT NULL,
                 display_name  TEXT
             );",
        )?;
        Ok(AuthStore {
            conn: Mutex::new(conn),
        })
    }

    /// Number of registered accounts.
    pub fn user_count(&self) -> i64 {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
            .unwrap_or(0)
    }

    /// Create or update an account, hashing `password` with Argon2.
    pub fn add_user(
        &self,
        username: &str,
        password: &str,
        display_name: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // password_hash::Error isn't std::error::Error on all feature sets;
        // surface it as a message.
        let hash = hash_password(password).map_err(|e| e.to_string())?;
        self.conn.lock().unwrap().execute(
            "INSERT INTO users (username, password_hash, display_name)
                 VALUES (?1, ?2, ?3)
             ON CONFLICT(username)
                 DO UPDATE SET password_hash = excluded.password_hash,
                               display_name  = excluded.display_name",
            params![username, hash, display_name],
        )?;
        Ok(())
    }

    /// Whether `password` matches the stored hash for `username`. Runs a hash
    /// even for unknown users so response timing doesn't reveal which usernames
    /// exist.
    pub fn verify(&self, username: &str, password: &str) -> bool {
        let stored: Option<String> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT password_hash FROM users WHERE username = ?1",
                [username],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);

        match stored {
            Some(hash) => verify_password(password, &hash),
            None => {
                let _ = hash_password(password);
                false
            }
        }
    }
}

/// Hash a password into an Argon2 PHC string with a fresh random salt.
fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

/// Constant-time verification of a password against a stored PHC hash.
fn verify_password(password: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .and_then(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed))
        .is_ok()
}
