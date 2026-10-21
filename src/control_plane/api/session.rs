use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use schemars::JsonSchema;
use serde::Serialize;
use uuid::Uuid;

use crate::model::{DEFAULT_IDLE_SECONDS, SERVER_MEMORY_BYTES, SERVER_STORAGE_BYTES};

use super::{ApiAccountHeaders, ApiError, AppState, internal::authenticated_user};

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    Suspended,
}

impl TryFrom<&str> for AccountStatus {
    type Error = ApiError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "active" => Ok(Self::Active),
            "suspended" => Ok(Self::Suspended),
            _ => Err(ApiError::with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid_account_status",
                "account has an invalid status",
            )),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Session {
    pub user: SessionUser,
    pub limits: ServiceLimits,
    /// Every server address is `<name>.<server_domain>`.
    pub server_domain: String,
    /// The account may read the operator dashboard and every server.
    pub is_admin: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SessionUser {
    pub id: Uuid,
    pub email: Option<String>,
    pub display_name: String,
    pub account_status: AccountStatus,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ServiceLimits {
    pub memory_bytes: i64,
    pub storage_bytes: i64,
    pub idle_seconds: i64,
    pub player_limit: Option<i64>,
}

pub(super) async fn session(
    State(state): State<Arc<AppState>>,
    headers: ApiAccountHeaders,
) -> Result<Json<Session>, ApiError> {
    let user = authenticated_user(&state, &headers).await?;
    let is_admin =
        user.account_status == "active" && super::admin::is_admin(&state, user.id).await?;
    Ok(Json(Session {
        user: SessionUser {
            id: user.id,
            email: user.email,
            display_name: user.display_name,
            account_status: AccountStatus::try_from(user.account_status.as_str())?,
        },
        limits: ServiceLimits {
            memory_bytes: SERVER_MEMORY_BYTES,
            storage_bytes: SERVER_STORAGE_BYTES,
            idle_seconds: DEFAULT_IDLE_SECONDS,
            player_limit: None,
        },
        server_domain: state.config.root_domain.clone(),
        is_admin,
    }))
}
