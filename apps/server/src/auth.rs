use anyhow::{anyhow, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand_core::{OsRng, RngCore};
use sqlx::SqlitePool;

/// A persisted user (row from `users`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    #[allow(dead_code)]
    pub email: String,
    pub password_hash: String,
    pub avatar_url: Option<String>,
    pub is_admin: i64,
    pub is_banned: i64,
}

/// Hash a plaintext password with Argon2id + a random salt.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("hash error: {e}"))?
        .to_string();
    Ok(hash)
}

/// Verify a plaintext password against a stored PHC hash.
pub fn verify_password(password: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Generate an opaque 256-bit session token (hex).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Resolve the user owning a session token, if the token is valid. Banned users
/// are treated as anonymous (their existing tokens stop resolving).
pub async fn user_for_token(pool: &SqlitePool, token: &str) -> Result<Option<User>> {
    let user = sqlx::query_as::<_, User>(
        "SELECT u.id, u.username, u.email, u.password_hash, u.avatar_url, u.is_admin, u.is_banned \
         FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token = ? AND u.is_banned = 0",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;
    Ok(user)
}
