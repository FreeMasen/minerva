//! User accounts and password verification, backed by SQLite (via sqlx).
//!
//! Passwords are hashed with Argon2 (RustCrypto) and stored as PHC strings;
//! verification uses Argon2's constant-time comparison. The Argon2 work is
//! deliberately slow, so it runs on a blocking thread. Account management is
//! done out-of-band via the `adduser` CLI subcommand.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use rand_core::OsRng;
use sqlx::SqlitePool;

/// A store of user accounts backed by a SQLite database.
pub struct AuthStore {
    pool: SqlitePool,
}

impl AuthStore {
    /// Wrap a shared connection pool.
    pub fn new(pool: SqlitePool) -> Self {
        AuthStore { pool }
    }

    /// Number of registered accounts.
    pub async fn user_count(&self) -> i64 {
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!: i64" FROM users"#)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0)
    }

    /// Create or update an account, hashing `password` with Argon2.
    pub async fn add_user(
        &self,
        username: &str,
        password: &str,
        display_name: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let owned = password.to_string();
        let hash = tokio::task::spawn_blocking(move || hash_password(&owned))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;

        sqlx::query!(
            "INSERT INTO users (username, password_hash, display_name)
                 VALUES (?, ?, ?)
             ON CONFLICT(username)
                 DO UPDATE SET password_hash = excluded.password_hash,
                               display_name  = excluded.display_name",
            username,
            hash,
            display_name,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether `password` matches the stored hash for `username`. Runs a hash
    /// even for unknown users so response timing doesn't reveal which usernames
    /// exist.
    pub async fn verify(&self, username: &str, password: &str) -> bool {
        let stored: Option<String> =
            sqlx::query_scalar!("SELECT password_hash FROM users WHERE username = ?", username)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();

        let password = password.to_string();
        tokio::task::spawn_blocking(move || match stored {
            Some(hash) => verify_password(&password, &hash),
            None => {
                let _ = hash_password(&password);
                false
            }
        })
        .await
        .unwrap_or(false)
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
