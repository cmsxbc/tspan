use axum::{
    http::{header, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use crate::db;
use crate::server::AppState;

#[derive(Clone)]
pub struct AuthConfig {
    pub web_username: String,
    pub web_password_hash: String,
}

pub fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer ").map(|s| s.to_string()))
}

pub fn verify_api_token_sync(state: &AppState, token: &str) -> Result<bool, StatusCode> {
    let mut conn = state.pool.lock().unwrap();
    db::verify_api_token(&mut conn, token).map_err(|e| {
        tracing::error!("DB error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

pub async fn check_api_auth(state: &AppState, headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    let token = extract_bearer_token(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let valid = verify_api_token_sync(state, &token)?;
    if valid {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

pub async fn check_web_auth(state: &AppState, headers: &axum::http::HeaderMap) -> Result<(), StatusCode> {
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());

    let authorized = match auth {
        Some(h) if h.starts_with("Basic ") => {
            let decoded = match BASE64.decode(&h[6..]) {
                Ok(d) => String::from_utf8_lossy(&d).to_string(),
                Err(_) => return Err(StatusCode::UNAUTHORIZED),
            };
            let split: Vec<&str> = decoded.splitn(2, ':').collect();
            if split.len() != 2 {
                return Err(StatusCode::UNAUTHORIZED);
            }
            verify_basic_auth(split[0], split[1], &state.auth)
        }
        _ => false,
    };

    if authorized {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn verify_basic_auth(username: &str, password: &str, config: &AuthConfig) -> bool {
    if username != config.web_username {
        return false;
    }
    if config.web_password_hash.starts_with("$2") {
        bcrypt::verify(password, &config.web_password_hash).unwrap_or(false)
    } else {
        password == config.web_password_hash
    }
}
