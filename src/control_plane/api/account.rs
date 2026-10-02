use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use uuid::Uuid;

use crate::model::ObservedState;

use super::actor::{AccountSessionPath, ServerIdPath};
use super::{ApiAccountHeaders, ApiError, ApiPath, AppState, auth, internal::authenticated_user};

#[derive(Debug, Serialize, JsonSchema)]
pub struct Account {
    pub identities: Vec<AccountIdentity>,
    pub sessions: Vec<AccountSession>,
    pub worlds: Vec<AccountWorld>,
}

#[derive(Debug, Serialize, sqlx::FromRow, JsonSchema)]
pub struct AccountWorld {
    pub id: Uuid,
    pub subdomain: String,
    pub observed_state: ObservedState,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow, JsonSchema)]
pub struct AccountIdentity {
    pub id: Uuid,
    pub provider: String,
    pub display_name: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
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
    headers: ApiAccountHeaders,
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
    let worlds = sqlx::query_as::<_, AccountWorld>(
        "SELECT id, subdomain, observed_state, is_default, created_at
         FROM servers WHERE owner_id = $1 AND desired_state <> 'deleted'
         ORDER BY is_default DESC, created_at, id",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(Account {
        identities,
        sessions,
        worlds,
    }))
}

pub(super) async fn make_default_world(
    State(state): State<Arc<AppState>>,
    headers: ApiAccountHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<StatusCode, ApiError> {
    let server_id = path.server_id;
    let user = authenticated_user(&state, &headers).await?;
    let mut tx = state.pool.begin().await?;
    let account_status: Option<String> =
        sqlx::query_scalar("SELECT account_status FROM users WHERE id = $1 FOR UPDATE")
            .bind(user.id)
            .fetch_optional(&mut *tx)
            .await?;
    if account_status.as_deref() != Some("active") {
        return Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "account_suspended",
            "account is suspended",
        ));
    }
    let owned_world: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM servers
         WHERE id = $1 AND owner_id = $2 AND desired_state <> 'deleted'
         FOR UPDATE",
    )
    .bind(server_id)
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?;
    if owned_world.is_none() {
        return Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "world_not_found",
            "world not found",
        ));
    }

    sqlx::query("UPDATE servers SET is_default = false WHERE owner_id = $1 AND is_default")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE servers SET is_default = true WHERE id = $1")
        .bind(server_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn revoke_session(
    State(state): State<Arc<AppState>>,
    headers: ApiAccountHeaders,
    ApiPath(path): ApiPath<AccountSessionPath>,
) -> Result<Response, ApiError> {
    let session_id = path.session_id;
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
