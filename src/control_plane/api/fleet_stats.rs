use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};

use super::{ApiError, AppState};

/// The whole service at a glance, for the landing page. It needs no session and names
/// no server: it is the same for everyone, so a cache may keep it for a few seconds.
pub(super) async fn fleet_stats(State(state): State<Arc<AppState>>) -> Result<Response, ApiError> {
    let stats = crate::fleet_stats::fleet_stats(&state.pool).await?;
    let mut response = Json(stats).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=10"),
    );
    Ok(response)
}
