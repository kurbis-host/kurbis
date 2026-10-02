use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use object_store::{ObjectStore, path::Path as ObjectPath};
use uuid::Uuid;

use crate::snapshot::{WorkerObjectKey, physical_object_path};

use super::{ApiError, AppState, worker_auth};
mod transfer;
use transfer::{
    body_with_reauthorization, delete_staged_object, publish_staged_object, storage_gc_guard,
    stream_object_body,
};

pub(super) mod authorization;
use authorization::{
    Assignment, AssignmentQuery, invalid_assignment, validate_key_authority,
    validate_key_authority_locked,
};

pub(super) async fn object(
    State(state): State<Arc<AppState>>,
    Path((server_id, key)): Path<(Uuid, String)>,
    Query(query): Query<AssignmentQuery>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let headers = request.headers().clone();
    let worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    let assignment = Assignment::from_query(query)?;
    if assignment.generation() <= 0 {
        return Err(invalid_assignment());
    }
    let key = WorkerObjectKey::parse(&key).map_err(|_| invalid_object_key())?;
    let method = request.method().clone();
    let object_path =
        physical_object_path(server_id, key.path().as_ref()).map_err(|_| invalid_object_key())?;

    match method {
        Method::HEAD => {
            let mut tx = state.pool.begin().await?;
            validate_key_authority_locked(
                &mut tx,
                worker_id,
                &headers,
                server_id,
                assignment,
                &key,
                &Method::HEAD,
            )
            .await?;
            let metadata = state
                .object_store
                .head(&object_path)
                .await
                .map_err(storage_error)?;
            if metadata.size > key.max_bytes() {
                return Err(storage_error_message());
            }
            tx.commit().await?;
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, metadata.size)
                .header(header::CACHE_CONTROL, "private, no-store")
                .body(Body::empty())
                .map_err(|_| storage_error_message())?;
            if let Some(e_tag) = metadata.e_tag
                && let Ok(e_tag) = HeaderValue::from_str(&e_tag)
            {
                response.headers_mut().insert(header::ETAG, e_tag);
            }
            Ok(response)
        }
        Method::GET => {
            validate_key_authority(
                &state.pool,
                worker_id,
                server_id,
                assignment,
                &key,
                &Method::GET,
            )
            .await?;
            let object = state
                .object_store
                .get(&object_path)
                .await
                .map_err(storage_error)?;
            let metadata = object.meta.clone();
            if metadata.size > key.max_bytes() {
                return Err(storage_error_message());
            }
            let body = body_with_reauthorization(
                object.into_stream(),
                state.pool.clone(),
                headers,
                worker_id,
                server_id,
                assignment,
                key.clone(),
                metadata.size,
            );
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, metadata.size)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(header::CACHE_CONTROL, "private, no-store")
                .body(body)
                .map_err(|_| storage_error_message())?;
            if let Some(e_tag) = metadata.e_tag
                && let Ok(e_tag) = HeaderValue::from_str(&e_tag)
            {
                response.headers_mut().insert(header::ETAG, e_tag);
            }
            Ok(response)
        }
        Method::PUT => {
            validate_key_authority(
                &state.pool,
                worker_id,
                server_id,
                assignment,
                &key,
                &Method::PUT,
            )
            .await?;
            let _gc_guard = storage_gc_guard(&state.pool).await?;
            let staged_path = ObjectPath::from(format!(
                "control-plane-object-staging/{server_id}/{}",
                Uuid::new_v4()
            ));
            let body = request.into_body();
            if let Err(error) = stream_object_body(
                state.object_store.as_ref(),
                &staged_path,
                key.max_bytes(),
                body,
            )
            .await
            {
                delete_staged_object(&state, &staged_path).await;
                return Err(error);
            }
            let publish = publish_staged_object(
                &state,
                &headers,
                worker_id,
                server_id,
                assignment,
                &key,
                &staged_path,
                &object_path,
            )
            .await;
            delete_staged_object(&state, &staged_path).await;
            publish?;
            Ok((
                StatusCode::NO_CONTENT,
                [(header::CACHE_CONTROL, "private, no-store")],
            )
                .into_response())
        }
        Method::DELETE => {
            let mut tx = state.pool.begin().await?;
            validate_key_authority_locked(
                &mut tx,
                worker_id,
                &headers,
                server_id,
                assignment,
                &key,
                &Method::DELETE,
            )
            .await?;
            state
                .object_store
                .delete(&object_path)
                .await
                .map_err(storage_error)?;
            tx.commit().await?;
            Ok((
                StatusCode::NO_CONTENT,
                [(header::CACHE_CONTROL, "private, no-store")],
            )
                .into_response())
        }
        _ => Err(ApiError::with_code(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "object method is not supported",
        )),
    }
}

fn invalid_object_key() -> ApiError {
    ApiError::with_code(
        StatusCode::BAD_REQUEST,
        "invalid_object_key",
        "object key is invalid",
    )
}

fn object_too_large() -> ApiError {
    ApiError::with_code(
        StatusCode::PAYLOAD_TOO_LARGE,
        "object_too_large",
        "object exceeds the allowed size",
    )
}

fn transfer_timeout() -> ApiError {
    ApiError::with_code(
        StatusCode::REQUEST_TIMEOUT,
        "object_transfer_timeout",
        "object transfer exceeded its time limit",
    )
}

fn storage_error(error: object_store::Error) -> ApiError {
    if matches!(&error, object_store::Error::NotFound { .. }) {
        ApiError::with_code(
            StatusCode::NOT_FOUND,
            "object_not_found",
            "object not found",
        )
    } else {
        tracing::warn!(?error, "worker object storage request failed");
        storage_error_message()
    }
}

fn storage_error_message() -> ApiError {
    ApiError::with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        "object_storage_unavailable",
        "object storage is unavailable",
    )
}
