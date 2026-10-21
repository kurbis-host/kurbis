use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use crate::snapshot::physical_object_path;

use super::actor::{ApiPath, ApiServerHeaders, ServerExportPath, ServerIdPath};
use super::internal::{require_server_access, require_server_view};
use super::{ApiError, AppState, lifecycle_error};

#[derive(Debug, Clone, Serialize, FromRow, JsonSchema)]
pub(super) struct WorldExport {
    id: Uuid,
    server_id: Uuid,
    status: String,
    stage: String,
    source_generation: Option<i64>,
    #[serde(skip_serializing)]
    #[schemars(skip)]
    archive_key: Option<String>,
    archive_size: Option<i64>,
    archive_checksum: Option<String>,
    last_error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

async fn export_by_id(
    state: &AppState,
    server_id: Uuid,
    export_id: Uuid,
) -> Result<WorldExport, ApiError> {
    sqlx::query_as::<_, WorldExport>(
        "SELECT id, server_id, status, stage, source_generation, archive_key, archive_size,
                archive_checksum, last_error, created_at, updated_at, expires_at
         FROM world_exports WHERE id = $1 AND server_id = $2",
    )
    .bind(export_id)
    .bind(server_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        ApiError::with_code(
            StatusCode::NOT_FOUND,
            "export_not_found",
            "world export not found",
        )
    })
}

pub(super) async fn list_exports(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<Json<Vec<WorldExport>>, ApiError> {
    let server_id = path.server_id;
    require_server_view(&state, &headers, server_id).await?;
    let exports = sqlx::query_as::<_, WorldExport>(
        "SELECT id, server_id, status, stage, source_generation, archive_key, archive_size,
                archive_checksum, last_error, created_at, updated_at, expires_at
         FROM world_exports WHERE server_id = $1 ORDER BY created_at DESC LIMIT 20",
    )
    .bind(server_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(exports))
}

pub(super) async fn create_export(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerIdPath>,
) -> Result<(StatusCode, Json<WorldExport>), ApiError> {
    let server_id = path.server_id;
    let server = require_server_access(&state, &headers, server_id).await?;
    let user_id = server.owner_id;
    let export_id = state
        .lifecycle
        .request_world_export(server.id, user_id)
        .await
        .map_err(lifecycle_error)?;
    let export = export_by_id(&state, server.id, export_id).await?;
    Ok((StatusCode::ACCEPTED, Json(export)))
}

pub(super) async fn get_export(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerExportPath>,
) -> Result<Json<WorldExport>, ApiError> {
    let server_id = path.server_id;
    let export_id = path.export_id;
    require_server_view(&state, &headers, server_id).await?;
    Ok(Json(export_by_id(&state, server_id, export_id).await?))
}

pub(super) async fn cancel_export(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerExportPath>,
) -> Result<Json<WorldExport>, ApiError> {
    let server_id = path.server_id;
    let export_id = path.export_id;
    require_server_access(&state, &headers, server_id).await?;
    let result = sqlx::query(
        "UPDATE world_exports SET status = 'cancelled', stage = 'cancelled', updated_at = now()
         WHERE id = $1 AND server_id = $2 AND status IN ('pending', 'running')",
    )
    .bind(export_id)
    .bind(server_id)
    .execute(&state.pool)
    .await?;
    let export = export_by_id(&state, server_id, export_id).await?;
    if result.rows_affected() == 0 && export.status != "cancelled" {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "export_cancel_conflict",
            "world export could not be cancelled",
        ));
    }
    Ok(Json(export))
}

pub(super) async fn download_export(
    State(state): State<Arc<AppState>>,
    headers: ApiServerHeaders,
    ApiPath(path): ApiPath<ServerExportPath>,
) -> Result<Response, ApiError> {
    let server_id = path.server_id;
    let export_id = path.export_id;
    let server = require_server_view(&state, &headers, server_id).await?;
    let export = export_by_id(&state, server_id, export_id).await?;
    if export.status != "completed" || export.expires_at.is_none_or(|expiry| expiry <= Utc::now()) {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "export_not_ready",
            "world export is not available for download",
        ));
    }
    let key = export
        .archive_key
        .as_deref()
        .filter(|key| {
            key.strip_prefix(&format!("exports/{export_id}/"))
                .and_then(|attempt| attempt.strip_suffix(".zip"))
                .is_some_and(|attempt| {
                    !attempt.is_empty() && attempt.bytes().all(|byte| byte.is_ascii_digit())
                })
        })
        .ok_or_else(|| {
            ApiError::with_code(
                StatusCode::INTERNAL_SERVER_ERROR,
                "export_archive_invalid",
                "world export archive path is invalid",
            )
        })?;
    let object_path = physical_object_path(server_id, key).map_err(|_| {
        ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "export_archive_invalid",
            "world export archive path is invalid",
        )
    })?;
    let archive = state.object_store.get(&object_path).await.map_err(|_| {
        ApiError::with_code(
            StatusCode::GONE,
            "export_archive_missing",
            "world export archive has expired",
        )
    })?;
    if export
        .archive_size
        .is_some_and(|size| size != archive.meta.size as i64)
    {
        return Err(ApiError::with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "export_archive_invalid",
            "world export archive failed integrity validation",
        ));
    }
    let archive_size = archive.meta.size;
    let mut response = Response::new(Body::from_stream(
        archive.into_stream().map_err(std::io::Error::other),
    ));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename=\"{}-world.zip\"",
            server.subdomain
        ))
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "invalid archive name"))?,
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&archive_size.to_string()).map_err(|_| {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "invalid archive size")
        })?,
    );
    Ok(response)
}
