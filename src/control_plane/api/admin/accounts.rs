use std::sync::Arc;

use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::{ApiError, ApiPath, ApiQuery, AppState};
use super::servers::{AdminServer, SERVER_COLUMNS, SERVER_JOINS};
use super::{AdminUser, Page, is_admin};

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct AccountListQuery {
    /// Matches account and identity names and emails, or an account id exactly.
    search: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// A linked sign-in, as the list shows it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub(super) struct AdminIdentitySummary {
    provider: String,
    display_name: String,
    email: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminAccount {
    id: Uuid,
    display_name: String,
    account_status: String,
    created_at: DateTime<Utc>,
    /// The latest request of any of the account's browser sessions.
    last_seen_at: Option<DateTime<Utc>>,
    #[sqlx(json)]
    identities: Vec<AdminIdentitySummary>,
    servers: i64,
    ai_cost_this_month_micros: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminAccountPage {
    total: i64,
    items: Vec<AdminAccount>,
}

const ACCOUNT_COLUMNS: &str =
    "users.id, users.display_name, users.account_status, users.created_at,
       (SELECT max(last_seen_at) FROM sessions WHERE sessions.user_id = users.id) AS last_seen_at,
       (SELECT coalesce(jsonb_agg(jsonb_build_object(
                   'provider', provider, 'display_name', display_name, 'email', email)
                ORDER BY created_at), '[]'::jsonb)
        FROM identities WHERE identities.user_id = users.id) AS identities,
       (SELECT count(*) FROM servers
        WHERE servers.owner_id = users.id AND servers.desired_state <> 'deleted') AS servers,
       (SELECT coalesce(sum(cost_micros), 0)::bigint FROM ai_usage
        WHERE ai_usage.owner_id = users.id
          AND ai_usage.created_at >= date_trunc('month', now())) AS ai_cost_this_month_micros";

pub(super) async fn accounts(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ApiQuery(query): ApiQuery<AccountListQuery>,
) -> Result<Json<AdminAccountPage>, ApiError> {
    let page = Page::new(query.search.as_deref(), query.limit, query.offset);
    let filter =
        "WHERE $1::text IS NULL OR users.display_name ILIKE $1 OR users.id::text = btrim($2)
                   OR EXISTS(SELECT 1 FROM identities WHERE identities.user_id = users.id
                             AND (identities.display_name ILIKE $1 OR identities.email ILIKE $1
                                  OR identities.subject = btrim($2)))";
    let total = sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM users {filter}"))
        .bind(&page.pattern)
        .bind(&page.search)
        .fetch_one(&state.pool)
        .await?;
    let items = sqlx::query_as::<_, AdminAccount>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM users {filter}
         ORDER BY users.created_at DESC, users.id LIMIT $3 OFFSET $4"
    ))
    .bind(&page.pattern)
    .bind(&page.search)
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(AdminAccountPage { total, items }))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(super) struct AccountIdPath {
    account_id: Uuid,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminIdentity {
    provider: String,
    /// The provider's id for the account; Minecraft identities use the player's UUID.
    subject: String,
    display_name: String,
    email: Option<String>,
    email_verified: bool,
    minecraft_player_id: Option<Uuid>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub(super) struct AdminSession {
    id: Uuid,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct AdminAccountDetail {
    account: AdminAccount,
    /// Whether the account may read this dashboard.
    is_admin: bool,
    identities: Vec<AdminIdentity>,
    /// Browser sessions, latest first.
    sessions: Vec<AdminSession>,
    servers: Vec<AdminServer>,
    ai_cost_total_micros: i64,
}

pub(super) async fn account(
    State(state): State<Arc<AppState>>,
    _admin: AdminUser,
    ApiPath(path): ApiPath<AccountIdPath>,
) -> Result<Json<AdminAccountDetail>, ApiError> {
    let pool = &state.pool;
    let account = sqlx::query_as::<_, AdminAccount>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM users WHERE users.id = $1"
    ))
    .bind(path.account_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| {
        ApiError::with_code(
            axum::http::StatusCode::NOT_FOUND,
            "account_not_found",
            "account not found",
        )
    })?;
    let identities = sqlx::query_as::<_, AdminIdentity>(
        "SELECT provider, subject, display_name, email, email_verified, minecraft_player_id, created_at
         FROM identities WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(account.id)
    .fetch_all(pool);
    let sessions = sqlx::query_as::<_, AdminSession>(
        "SELECT id, created_at, last_seen_at FROM sessions
         WHERE user_id = $1 ORDER BY last_seen_at DESC",
    )
    .bind(account.id)
    .fetch_all(pool);
    let servers_query = format!(
        "SELECT {SERVER_COLUMNS} FROM servers {SERVER_JOINS}
         WHERE servers.owner_id = $1 AND servers.desired_state <> 'deleted'
         ORDER BY servers.created_at"
    );
    let servers = sqlx::query_as::<_, AdminServer>(&servers_query)
        .bind(account.id)
        .fetch_all(pool);
    let ai_cost_total = sqlx::query_scalar::<_, i64>(
        "SELECT coalesce(sum(cost_micros), 0)::bigint FROM ai_usage WHERE owner_id = $1",
    )
    .bind(account.id)
    .fetch_one(pool);
    let (identities, sessions, servers, ai_cost_total_micros, is_admin) = tokio::try_join!(
        identities,
        sessions,
        servers,
        ai_cost_total,
        is_admin(&state, account.id)
    )?;
    Ok(Json(AdminAccountDetail {
        account,
        is_admin,
        identities,
        sessions,
        servers,
        ai_cost_total_micros,
    }))
}
