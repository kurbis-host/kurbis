use axum::http::{HeaderMap, StatusCode, header};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{ApiError, AppState, auth};

pub(super) async fn authenticated_worker_id(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Uuid, ApiError> {
    authenticated_worker_identity(state, headers)
        .await
        .map(|(worker_id, _)| worker_id)
}

pub(super) async fn authenticated_worker_identity(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Uuid, Uuid), ApiError> {
    let token = worker_token(headers)?;
    let lease_id = worker_lease_id(headers)?;
    let worker: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT worker_id, lease_id FROM worker_credentials
         WHERE api_token_hash = $1 AND revoked_at IS NULL
           AND lease_id = $2 AND lease_expires_at > now()",
    )
    .bind(auth::token_hash(token))
    .bind(lease_id)
    .fetch_optional(&state.pool)
    .await?;
    worker.ok_or_else(invalid_worker_token)
}

pub(super) async fn lock_authenticated_worker_id(
    tx: &mut Transaction<'_, Postgres>,
    headers: &HeaderMap,
) -> Result<Uuid, ApiError> {
    let token = worker_token(headers)?;
    let lease_id = worker_lease_id(headers)?;
    let worker_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT worker_id FROM worker_credentials
         WHERE api_token_hash = $1 AND revoked_at IS NULL
           AND lease_id = $2 AND lease_expires_at > now()
         FOR SHARE",
    )
    .bind(auth::token_hash(token))
    .bind(lease_id)
    .fetch_optional(&mut **tx)
    .await?;
    worker_id.ok_or_else(invalid_worker_token)
}

pub(super) async fn lock_authenticated_worker_id_for_lease(
    tx: &mut Transaction<'_, Postgres>,
    headers: &HeaderMap,
) -> Result<Uuid, ApiError> {
    let token = worker_token(headers)?;
    let worker_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT worker_id FROM worker_credentials
         WHERE api_token_hash = $1 AND revoked_at IS NULL
         FOR UPDATE",
    )
    .bind(auth::token_hash(token))
    .fetch_optional(&mut **tx)
    .await?;
    worker_id.ok_or_else(invalid_worker_token)
}

pub(super) fn worker_lease_id(headers: &HeaderMap) -> Result<Uuid, ApiError> {
    headers
        .get("x-worker-lease")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(invalid_worker_token)
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
