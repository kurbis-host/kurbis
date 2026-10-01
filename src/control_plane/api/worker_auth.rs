use axum::http::{HeaderMap, StatusCode, header};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{ApiError, AppState, auth};

pub(super) async fn authenticated_worker_id(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Uuid, ApiError> {
    let token = worker_token(headers)?;
    let worker_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT worker_id FROM worker_auth.credentials
         WHERE api_token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(auth::token_hash(token))
    .fetch_optional(&state.pool)
    .await?;
    worker_id.ok_or_else(invalid_worker_token)
}

pub(super) async fn lock_authenticated_worker_id(
    tx: &mut Transaction<'_, Postgres>,
    headers: &HeaderMap,
) -> Result<Uuid, ApiError> {
    let token = worker_token(headers)?;
    let worker_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT worker_id FROM worker_auth.credentials
         WHERE api_token_hash = $1 AND revoked_at IS NULL
         FOR SHARE",
    )
    .bind(auth::token_hash(token))
    .fetch_optional(&mut **tx)
    .await?;
    worker_id.ok_or_else(invalid_worker_token)
}

pub(super) fn invalid_worker_token() -> ApiError {
    ApiError::with_code(
        StatusCode::UNAUTHORIZED,
        "invalid_worker_token",
        "worker authentication failed",
    )
}

fn valid_worker_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn worker_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| valid_worker_token(value))
        .ok_or_else(invalid_worker_token)
}
