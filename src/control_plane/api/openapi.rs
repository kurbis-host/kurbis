use std::sync::Arc;

use aide::openapi::OpenApi;
use axum::{Extension, Json};

pub(super) async fn serve(Extension(document): Extension<Arc<OpenApi>>) -> Json<OpenApi> {
    Json((*document).clone())
}
