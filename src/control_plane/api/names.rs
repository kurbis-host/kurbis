use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::validate_subdomain;

use super::{ApiError, ApiPath, AppState};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct NamePath {
    pub name: String,
}

/// Whether a server address is still free to take, asked before anyone has an account.
#[derive(Debug, Serialize, JsonSchema)]
pub struct NameAvailability {
    pub name: String,
    pub free: bool,
}

pub(super) async fn name_availability(
    State(state): State<Arc<AppState>>,
    ApiPath(path): ApiPath<NamePath>,
) -> Result<Json<NameAvailability>, ApiError> {
    if !validate_subdomain(&path.name) {
        return Err(ApiError::with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_subdomain",
            "a name is 3 to 32 lowercase letters, digits or dashes, with no dash at either end",
        ));
    }
    // Any row holds the name: a server being deleted keeps it (the column is unique)
    // until its row is removed.
    let taken: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM servers WHERE subdomain = $1)")
            .bind(&path.name)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(NameAvailability {
        name: path.name,
        free: !taken,
    }))
}
