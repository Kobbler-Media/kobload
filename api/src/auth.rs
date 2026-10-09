use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::http::{HeaderMap, StatusCode};
use password_hash::SaltString;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::repositories::auth as auth_repo;
use crate::state::AppState;

pub async fn require_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let auth = headers
        .get("authorization")
        .and_then(|val| val.to_str().ok())
        .unwrap_or("");
    let token = auth.strip_prefix("Bearer ").unwrap_or("").trim();
    if token.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    validate_token(state, token).await
}

pub async fn require_auth_with_token(
    state: &AppState,
    headers: &HeaderMap,
    token: Option<&str>,
) -> Result<(), StatusCode> {
    if let Some(token) = token
        && !token.trim().is_empty()
    {
        return validate_token(state, token.trim()).await;
    }
    require_auth(state, headers).await
}

/// Avoids leaking the configured API key byte-by-byte through compare timing.
fn constant_time_str_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

pub async fn validate_token(state: &AppState, token: &str) -> Result<(), StatusCode> {
    // Check environment variable API key first
    if let Some(expected) = state.api_key.as_ref()
        && constant_time_str_eq(token, expected)
    {
        return Ok(());
    }

    // Check auth_tokens (user login sessions)
    let exists = auth_repo::auth_token_exists(&state.pool, token)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if exists {
        return Ok(());
    }

    // Check api_keys table (named API keys)
    let key_hash = hash_api_key(token);
    let api_key_exists = auth_repo::api_key_exists_by_hash(&state.pool, &key_hash)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if api_key_exists {
        // Update last_used_at asynchronously (fire-and-forget)
        let pool = state.pool.clone();
        let hash = key_hash.clone();
        tokio::spawn(async move {
            let _ = auth_repo::touch_api_key(&pool, &hash).await;
        });
        return Ok(());
    }

    Err(StatusCode::UNAUTHORIZED)
}

pub fn hash_password(password: &str) -> Result<String, password_hash::Error> {
    let salt = SaltString::generate(&mut password_hash::rand_core::OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn hash_api_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    const_hex::encode(hasher.finalize())
}

/// Extract the API key ID from the request if a named API key was used.
/// Returns None if env var API key, session token, or no auth was used.
pub async fn get_api_key_id(state: &AppState, headers: &HeaderMap) -> Option<Uuid> {
    let auth = headers
        .get("authorization")
        .and_then(|val| val.to_str().ok())
        .unwrap_or("");
    let token = auth.strip_prefix("Bearer ").unwrap_or("").trim();
    if token.is_empty() {
        return None;
    }

    // Check if it's a named API key
    let key_hash = hash_api_key(token);
    auth_repo::get_api_key_id_by_hash(&state.pool, &key_hash)
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_and_verify_password() {
        let password = "kobload-secret";
        let hash = hash_password(password).expect("hash");
        assert!(verify_password(password, &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn constant_time_str_eq_matches_only_identical_strings() {
        assert!(constant_time_str_eq("my-api-key", "my-api-key"));
        assert!(!constant_time_str_eq("my-api-key", "my-api-ke0"));
        assert!(!constant_time_str_eq("my-api-key", "shorter"));
        assert!(!constant_time_str_eq(
            "my-api-key",
            "much-longer-than-the-key"
        ));
        assert!(!constant_time_str_eq("my-api-key", ""));
    }
}
