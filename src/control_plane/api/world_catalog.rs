use axum::{
    Json,
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};

use crate::contracts::world::CATALOG;

/// What a new world can be made of. It only changes with the pinned Pumpkin,
/// so a cache may keep it for a day.
pub(super) async fn world_catalog() -> Response {
    let mut response = Json(&CATALOG.catalog).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    response
}
