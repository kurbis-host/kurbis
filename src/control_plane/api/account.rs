use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::{ApiError, AppState, auth, internal::authenticated_user};

#[derive(Debug, Serialize)]
pub struct Account {
    pub identities: Vec<AccountIdentity>,
    pub sessions: Vec<AccountSession>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AccountIdentity {
    pub id: Uuid,
    pub provider: String,
    pub display_name: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct AccountSession {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub current: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    token_hash: Vec<u8>,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
}

pub(super) async fn account(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Account>, ApiError> {
    let user = authenticated_user(&state, &headers).await?;
    let identities = sqlx::query_as::<_, AccountIdentity>(
        "SELECT id, provider, display_name, email, email_verified, created_at
         FROM identities WHERE user_id = $1 ORDER BY created_at, id",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    let current_hash = auth::current_session_hash(&headers);
    let sessions = sqlx::query_as::<_, SessionRow>(
        "SELECT id, token_hash, created_at, last_seen_at
         FROM sessions WHERE user_id = $1 ORDER BY last_seen_at DESC, id",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|session| AccountSession {
        id: session.id,
        created_at: session.created_at,
        last_seen_at: session.last_seen_at,
        current: current_hash.as_deref() == Some(session.token_hash.as_slice()),
    })
    .collect();
    Ok(Json(Account {
        identities,
        sessions,
    }))
}

pub(super) async fn revoke_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let user = authenticated_user(&state, &headers).await?;
    let current_hash = auth::current_session_hash(&headers);
    let revoked: Option<Vec<u8>> = sqlx::query_scalar(
        "DELETE FROM sessions WHERE id = $1 AND user_id = $2 RETURNING token_hash",
    )
    .bind(session_id)
    .bind(user.id)
    .fetch_optional(&state.pool)
    .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    if revoked
        .as_deref()
        .is_some_and(|hash| current_hash.as_deref() == Some(hash))
    {
        response.headers_mut().append(
            header::SET_COOKIE,
            "kurbis_session=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0"
                .parse()
                .expect("static cookie is valid"),
        );
    }
    Ok(response)
}
