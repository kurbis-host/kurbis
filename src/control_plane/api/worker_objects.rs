use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::{Bytes, BytesMut};
use futures::{StreamExt, stream};
use object_store::{MultipartUpload, ObjectStore, path::Path as ObjectPath};
use serde::Deserialize;
use sqlx::{PgPool, Postgres, Transaction, pool::PoolConnection};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::model::SERVER_STORAGE_BYTES;
use crate::worker::snapshot::{ObjectKind, WorkerObjectKey, physical_object_path};

use super::{ApiError, AppState, worker_auth};

const MULTIPART_PART_BYTES: usize = 8 * 1024 * 1024;
const GET_AUTH_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const S3_SINGLE_COPY_LIMIT_BYTES: u64 = 5_000_000_000;
const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OBJECT_TRANSFER_TIME: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Debug, Deserialize)]
#[serde(tag = "assignment", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum AssignmentQuery {
    Placement {
        generation: String,
    },
    Operation {
        operation_id: String,
        generation: String,
    },
    Export {
        export_id: String,
        generation: String,
    },
}

#[derive(Debug, Clone, Copy)]
enum Assignment {
    Placement { generation: i64 },
    Operation { operation_id: Uuid, generation: i64 },
    Export { export_id: Uuid, generation: i64 },
}

impl Assignment {
    fn from_query(query: AssignmentQuery) -> Result<Self, ApiError> {
        match query {
            AssignmentQuery::Placement { generation } => Ok(Self::Placement {
                generation: parse_generation(&generation)?,
            }),
            AssignmentQuery::Operation {
                operation_id,
                generation,
            } => Ok(Self::Operation {
                operation_id: operation_id.parse().map_err(|_| invalid_assignment())?,
                generation: parse_generation(&generation)?,
            }),
            AssignmentQuery::Export {
                export_id,
                generation,
            } => Ok(Self::Export {
                export_id: export_id.parse().map_err(|_| invalid_assignment())?,
                generation: parse_generation(&generation)?,
            }),
        }
    }

    fn generation(self) -> i64 {
        match self {
            Self::Placement { generation }
            | Self::Operation { generation, .. }
            | Self::Export { generation, .. } => generation,
        }
    }
}

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

async fn validate_key_authority(
    pool: &PgPool,
    worker_id: Uuid,
    server_id: Uuid,
    assignment: Assignment,
    key: &WorkerObjectKey,
    method: &Method,
) -> Result<(), ApiError> {
    let allowed = match assignment {
        Assignment::Placement { generation } => {
            let current: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM placements p
                    JOIN servers s ON s.id = p.server_id
                    WHERE p.server_id = $1 AND p.worker_id = $2
                      AND p.generation = $3 AND s.generation = $3
                )",
            )
            .bind(server_id)
            .bind(worker_id)
            .bind(generation)
            .fetch_one(pool)
            .await?;
            current && is_read(method) && is_world_object(key.kind())
        }
        Assignment::Operation {
            operation_id,
            generation,
        } => {
            let operation: Option<(String, bool)> = sqlx::query_as(
                "SELECT o.kind::text,
                        EXISTS(SELECT 1 FROM placements p
                               WHERE p.server_id = s.id AND p.worker_id = $3
                                 AND p.generation = $4) AS has_current_placement
                 FROM operations o
                 JOIN servers s ON s.id = o.server_id
                 WHERE o.id = $1 AND o.server_id = $2 AND o.worker_id = $3
                   AND o.status IN ('pending', 'running')
                   AND o.generation = $4 AND s.generation = $4",
            )
            .bind(operation_id)
            .bind(server_id)
            .bind(worker_id)
            .bind(generation)
            .fetch_optional(pool)
            .await?;
            operation.is_some_and(|(kind, has_placement)| {
                (matches!(kind.as_str(), "reset" | "delete") || has_placement)
                    && operation_allows_object(&kind, key.kind(), method)
            })
        }
        Assignment::Export {
            export_id,
            generation,
        } => {
            let active_attempt: Option<(i32, String)> = sqlx::query_as(
                "SELECT e.attempt, e.stage FROM world_exports e
                 WHERE e.id = $1 AND e.server_id = $2 AND e.worker_id = $3
                   AND e.status IN ('pending', 'running') AND e.source_generation = $4",
            )
            .bind(export_id)
            .bind(server_id)
            .bind(worker_id)
            .bind(generation)
            .fetch_optional(pool)
            .await?;
            active_attempt.is_some_and(|(attempt, stage)| {
                export_allows_object(export_id, attempt, &stage, key, method)
            })
        }
    };
    if allowed {
        Ok(())
    } else {
        Err(ApiError::with_code(
            StatusCode::NOT_FOUND,
            "object_not_found",
            "object is not available to this worker",
        ))
    }
}

async fn validate_key_authority_locked(
    tx: &mut Transaction<'_, Postgres>,
    worker_id: Uuid,
    headers: &HeaderMap,
    server_id: Uuid,
    assignment: Assignment,
    key: &WorkerObjectKey,
    method: &Method,
) -> Result<(), ApiError> {
    let allowed = match assignment {
        Assignment::Placement { generation } => {
            let current_generation: Option<i64> =
                sqlx::query_scalar("SELECT generation FROM servers WHERE id = $1 FOR SHARE")
                    .bind(server_id)
                    .fetch_optional(&mut **tx)
                    .await?;
            let placement: Option<(Uuid, i64)> = sqlx::query_as(
                "SELECT worker_id, generation FROM placements
                 WHERE server_id = $1 FOR SHARE",
            )
            .bind(server_id)
            .fetch_optional(&mut **tx)
            .await?;
            current_generation == Some(generation)
                && placement.is_some_and(|(owner, placed_generation)| {
                    owner == worker_id && placed_generation == generation
                })
                && is_read(method)
                && is_world_object(key.kind())
        }
        Assignment::Operation {
            operation_id,
            generation,
        } => {
            let current_generation: Option<i64> =
                sqlx::query_scalar("SELECT generation FROM servers WHERE id = $1 FOR SHARE")
                    .bind(server_id)
                    .fetch_optional(&mut **tx)
                    .await?;
            let operation: Option<String> = sqlx::query_scalar(
                "SELECT kind::text FROM operations
                 WHERE id = $1 AND server_id = $2 AND worker_id = $3
                   AND status IN ('pending', 'running') AND generation = $4
                 FOR SHARE",
            )
            .bind(operation_id)
            .bind(server_id)
            .bind(worker_id)
            .bind(generation)
            .fetch_optional(&mut **tx)
            .await?;
            let Some(kind) = operation else {
                return Err(object_not_available());
            };
            let has_current_placement = if matches!(kind.as_str(), "reset" | "delete") {
                true
            } else {
                let placement: Option<(Uuid, i64)> = sqlx::query_as(
                    "SELECT worker_id, generation FROM placements
                     WHERE server_id = $1 FOR SHARE",
                )
                .bind(server_id)
                .fetch_optional(&mut **tx)
                .await?;
                placement.is_some_and(|(owner, placed_generation)| {
                    owner == worker_id && placed_generation == generation
                })
            };
            current_generation == Some(generation)
                && has_current_placement
                && operation_allows_object(&kind, key.kind(), method)
        }
        Assignment::Export {
            export_id,
            generation,
        } => {
            sqlx::query("SELECT id FROM servers WHERE id = $1 FOR SHARE")
                .bind(server_id)
                .fetch_optional(&mut **tx)
                .await?;
            let active_attempt: Option<(i32, String)> = sqlx::query_as(
                "SELECT attempt, stage FROM world_exports
                 WHERE id = $1 AND server_id = $2 AND worker_id = $3
                   AND status IN ('pending', 'running') AND source_generation = $4
                 FOR SHARE",
            )
            .bind(export_id)
            .bind(server_id)
            .bind(worker_id)
            .bind(generation)
            .fetch_optional(&mut **tx)
            .await?;
            active_attempt.is_some_and(|(attempt, stage)| {
                export_allows_object(export_id, attempt, &stage, key, method)
            })
        }
    };
    if allowed {
        let locked_worker_id = worker_auth::lock_authenticated_worker_id(tx, headers).await?;
        if locked_worker_id != worker_id {
            return Err(worker_auth::invalid_worker_token());
        }
        Ok(())
    } else {
        Err(object_not_available())
    }
}

async fn storage_gc_guard(pool: &PgPool) -> Result<PoolConnection<Postgres>, ApiError> {
    let mut connection = pool.acquire().await?;
    connection.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock_shared(724515, 0)")
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

#[allow(clippy::too_many_arguments)]
async fn publish_staged_object(
    state: &AppState,
    headers: &HeaderMap,
    worker_id: Uuid,
    server_id: Uuid,
    assignment: Assignment,
    key: &WorkerObjectKey,
    staged_path: &ObjectPath,
    object_path: &ObjectPath,
) -> Result<(), ApiError> {
    // Establish authority briefly before doing any object-store work. The
    // finalization copy is deliberately outside this transaction: an S3-side
    // multipart copy can take minutes and must not pin lifecycle rows.
    let mut tx = state.pool.begin().await?;
    validate_key_authority_locked(
        &mut tx,
        worker_id,
        headers,
        server_id,
        assignment,
        key,
        &Method::PUT,
    )
    .await?;
    tx.commit().await?;

    // Serialize final-key publication without holding assignment rows. Keep a
    // CP-only backup when replacing an existing object so a revoked request
    // can restore the previous bytes rather than leaving a stale overwrite.
    let _publish_guard = object_publish_guard(&state.pool, object_path).await?;
    let backup_path = ObjectPath::from(format!(
        "control-plane-object-staging/{server_id}/{}-backup",
        Uuid::new_v4()
    ));
    let had_existing = match state.object_store.head(object_path).await {
        Ok(metadata) => {
            if metadata.size > key.max_bytes() {
                return Err(storage_error_message());
            }
            state
                .object_store
                .copy(object_path, &backup_path)
                .await
                .map_err(storage_error)?;
            true
        }
        Err(object_store::Error::NotFound { .. }) => false,
        Err(error) => return Err(storage_error(error)),
    };
    if let Err(error) =
        publish_staged_bytes(state.object_store.as_ref(), staged_path, object_path).await
    {
        if had_existing {
            let _ = state.object_store.delete(&backup_path).await;
        }
        return Err(error);
    }

    let mut tx = state.pool.begin().await?;
    let authorization = validate_key_authority_locked(
        &mut tx,
        worker_id,
        headers,
        server_id,
        assignment,
        key,
        &Method::PUT,
    )
    .await;
    if let Err(error) = authorization {
        drop(tx);
        if had_existing {
            if let Err(restore_error) = state.object_store.copy(&backup_path, object_path).await {
                tracing::warn!(
                    ?restore_error,
                    ?object_path,
                    "failed to restore worker object after authorization revocation"
                );
            }
        } else {
            let _ = state.object_store.delete(object_path).await;
        }
        let _ = state.object_store.delete(&backup_path).await;
        return Err(error);
    }
    tx.commit().await?;
    if had_existing {
        let _ = state.object_store.delete(&backup_path).await;
    }
    Ok(())
}

async fn object_publish_guard(
    pool: &PgPool,
    object_path: &ObjectPath,
) -> Result<PoolConnection<Postgres>, ApiError> {
    let mut connection = pool.acquire().await?;
    connection.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock(724516, hashtext($1))")
        .bind(object_path.as_ref())
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

async fn publish_staged_bytes(
    store: &dyn ObjectStore,
    staged_path: &ObjectPath,
    object_path: &ObjectPath,
) -> Result<(), ApiError> {
    let metadata = store.head(staged_path).await.map_err(storage_error)?;
    if metadata.size > SERVER_STORAGE_BYTES as u64 {
        return Err(object_too_large());
    }
    if metadata.size <= S3_SINGLE_COPY_LIMIT_BYTES {
        return store
            .copy(staged_path, object_path)
            .await
            .map_err(storage_error);
    }

    let object = store.get(staged_path).await.map_err(storage_error)?;
    let expected_size = object.meta.size;
    let mut input = object.into_stream();
    let mut upload = store
        .put_multipart(object_path)
        .await
        .map_err(storage_error)?;
    let result = async {
        let mut buffer = BytesMut::with_capacity(MULTIPART_PART_BYTES);
        let mut total = 0_u64;
        while let Some(chunk) = input.next().await {
            let chunk = chunk.map_err(storage_error)?;
            total = total
                .checked_add(chunk.len() as u64)
                .ok_or_else(object_too_large)?;
            if total > expected_size {
                return Err(storage_error_message());
            }
            buffer.extend_from_slice(&chunk);
            while buffer.len() >= MULTIPART_PART_BYTES {
                let part = buffer.split_to(MULTIPART_PART_BYTES).freeze();
                upload.put_part(part.into()).await.map_err(storage_error)?;
            }
        }
        if !buffer.is_empty() {
            upload
                .put_part(buffer.freeze().into())
                .await
                .map_err(storage_error)?;
        }
        if total != expected_size {
            return Err(storage_error_message());
        }
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = result {
        let _ = upload.abort().await;
        return Err(error);
    }
    upload.complete().await.map_err(storage_error)?;
    Ok(())
}

async fn delete_staged_object(state: &AppState, path: &ObjectPath) {
    match state.object_store.delete(path).await {
        Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
        Err(error) => tracing::warn!(
            ?error,
            "failed to clean worker object upload staging object"
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn body_with_reauthorization(
    source: futures::stream::BoxStream<'static, object_store::Result<Bytes>>,
    pool: PgPool,
    headers: HeaderMap,
    worker_id: Uuid,
    server_id: Uuid,
    assignment: Assignment,
    key: WorkerObjectKey,
    expected_size: u64,
) -> Body {
    let (sender, receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        stream_authorized_object(
            source,
            sender,
            pool,
            headers,
            worker_id,
            server_id,
            assignment,
            key,
            expected_size,
        )
        .await;
    });
    Body::from_stream(stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    }))
}

#[allow(clippy::too_many_arguments)]
async fn stream_authorized_object(
    mut source: futures::stream::BoxStream<'static, object_store::Result<Bytes>>,
    sender: mpsc::Sender<Result<Bytes, std::io::Error>>,
    pool: PgPool,
    headers: HeaderMap,
    worker_id: Uuid,
    server_id: Uuid,
    assignment: Assignment,
    key: WorkerObjectKey,
    expected_size: u64,
) {
    let deadline = tokio::time::Instant::now() + MAX_OBJECT_TRANSFER_TIME;
    let mut total = 0_u64;
    let mut pending = None;
    loop {
        let mut buffer = BytesMut::with_capacity(GET_AUTH_CHUNK_BYTES);
        let mut eof = false;
        while buffer.len() < GET_AUTH_CHUNK_BYTES {
            let (chunk, from_source) = if let Some(chunk) = pending.take() {
                (chunk, false)
            } else {
                match tokio::time::timeout_at(
                    deadline,
                    tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, source.next()),
                )
                .await
                {
                    Ok(Ok(Some(Ok(chunk)))) => (chunk, true),
                    Ok(Ok(Some(Err(error)))) => {
                        send_body_error(&sender, std::io::Error::other(error.to_string()));
                        return;
                    }
                    Ok(Ok(None)) => {
                        eof = true;
                        break;
                    }
                    Ok(Err(_)) | Err(_) => {
                        send_body_error(&sender, transfer_timeout_io());
                        return;
                    }
                }
            };
            if from_source {
                total = match total.checked_add(chunk.len() as u64) {
                    Some(total) if total <= key.max_bytes() && total <= expected_size => total,
                    _ => {
                        send_body_error(
                            &sender,
                            std::io::Error::other("object stream exceeded its declared size"),
                        );
                        return;
                    }
                };
            }
            let room = GET_AUTH_CHUNK_BYTES - buffer.len();
            let take = room.min(chunk.len());
            buffer.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                pending = Some(chunk.slice(take..));
            }
        }

        if buffer.is_empty() && eof {
            if total != expected_size {
                send_body_error(
                    &sender,
                    std::io::Error::other("object stream ended before its declared size"),
                );
            }
            return;
        }
        if buffer.is_empty() {
            continue;
        }

        let permit = match tokio::time::timeout_at(
            deadline,
            tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, sender.reserve()),
        )
        .await
        {
            Ok(Ok(Ok(permit))) => permit,
            Ok(Ok(Err(_))) => return,
            Ok(Err(_)) | Err(_) => {
                send_body_error(&sender, transfer_timeout_io());
                return;
            }
        };

        let authorization = async {
            let mut tx = pool.begin().await.map_err(ApiError::from)?;
            validate_key_authority_locked(
                &mut tx,
                worker_id,
                &headers,
                server_id,
                assignment,
                &key,
                &Method::GET,
            )
            .await?;
            permit.send(Ok(buffer.freeze()));
            tx.commit().await.map_err(ApiError::from)
        };
        match tokio::time::timeout_at(
            deadline,
            tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, authorization),
        )
        .await
        {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(_))) => {
                send_body_error(
                    &sender,
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "worker object authorization failed",
                    ),
                );
                return;
            }
            Ok(Err(_)) | Err(_) => {
                send_body_error(&sender, transfer_timeout_io());
                return;
            }
        }

        if eof {
            if total != expected_size {
                send_body_error(
                    &sender,
                    std::io::Error::other("object stream ended before its declared size"),
                );
            }
            return;
        }
    }
}

fn send_body_error(sender: &mpsc::Sender<Result<Bytes, std::io::Error>>, error: std::io::Error) {
    let _ = sender.try_send(Err(error));
}

fn transfer_timeout_io() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "worker object transfer exceeded its idle or total time limit",
    )
}

fn object_not_available() -> ApiError {
    ApiError::with_code(
        StatusCode::NOT_FOUND,
        "object_not_found",
        "object is not available to this worker",
    )
}

fn operation_allows_object(kind: &str, object_kind: ObjectKind, method: &Method) -> bool {
    match object_kind {
        ObjectKind::Chunk => {
            let can_read = matches!(kind, "start" | "snapshot" | "stop" | "restore" | "reset");
            let can_write = matches!(kind, "snapshot" | "stop" | "restore" | "reset");
            (can_read && is_read(method)) || (can_write && *method == Method::PUT)
        }
        ObjectKind::Manifest { .. } => {
            let can_read = matches!(
                kind,
                "start" | "snapshot" | "stop" | "restore" | "reset" | "delete"
            );
            let can_write = matches!(kind, "snapshot" | "stop" | "restore" | "reset");
            let can_delete = can_write || kind == "delete";
            (can_read && is_read(method))
                || (can_write && *method == Method::PUT)
                || (can_delete && *method == Method::DELETE)
        }
        ObjectKind::Export { .. } => false,
    }
}

fn export_allows_object(
    export_id: Uuid,
    current_attempt: i32,
    stage: &str,
    key: &WorkerObjectKey,
    method: &Method,
) -> bool {
    match key.kind() {
        ObjectKind::Chunk | ObjectKind::Manifest { .. } => is_read(method),
        ObjectKind::Export {
            export_id: key_export_id,
            attempt,
        } => {
            key_export_id == export_id
                && attempt == current_attempt
                && (*method != Method::PUT || stage == "archiving")
                && (*method == Method::GET
                    || *method == Method::HEAD
                    || *method == Method::PUT
                    || *method == Method::DELETE)
        }
    }
}

fn is_read(method: &Method) -> bool {
    *method == Method::GET || *method == Method::HEAD
}

fn is_world_object(kind: ObjectKind) -> bool {
    matches!(kind, ObjectKind::Chunk | ObjectKind::Manifest { .. })
}

async fn stream_object_body(
    store: &dyn ObjectStore,
    path: &ObjectPath,
    max_bytes: u64,
    body: Body,
) -> Result<(), ApiError> {
    let mut input = body.into_data_stream();
    let mut buffer = BytesMut::with_capacity(MULTIPART_PART_BYTES);
    let mut total = 0_u64;
    let mut upload: Option<Box<dyn MultipartUpload>> = None;
    let deadline = tokio::time::Instant::now() + MAX_OBJECT_TRANSFER_TIME;
    loop {
        let chunk = match tokio::time::timeout_at(
            deadline,
            tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, input.next()),
        )
        .await
        {
            Ok(Ok(Some(Ok(chunk)))) => chunk,
            Ok(Ok(Some(Err(_)))) => {
                abort_upload(&mut upload, deadline).await;
                return Err(ApiError::with_code(
                    StatusCode::BAD_REQUEST,
                    "invalid_object_body",
                    "object body could not be read",
                ));
            }
            Ok(Ok(None)) => break,
            Ok(Err(_)) | Err(_) => {
                abort_upload(&mut upload, deadline).await;
                return Err(transfer_timeout());
            }
        };
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(object_too_large)?;
        if total > max_bytes {
            abort_upload(&mut upload, deadline).await;
            return Err(object_too_large());
        }
        buffer.extend_from_slice(&chunk);
        if buffer.len() >= MULTIPART_PART_BYTES {
            if upload.is_none() {
                let multipart = tokio::time::timeout_at(
                    deadline,
                    tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, store.put_multipart(path)),
                )
                .await;
                upload = Some(match multipart {
                    Ok(Ok(Ok(upload))) => upload,
                    Ok(Ok(Err(error))) => return Err(storage_error(error)),
                    Ok(Err(_)) | Err(_) => return Err(transfer_timeout()),
                });
            }
            while buffer.len() >= MULTIPART_PART_BYTES {
                let part = buffer.split_to(MULTIPART_PART_BYTES).freeze();
                let put_part = upload
                    .as_mut()
                    .expect("multipart upload was just initialized")
                    .put_part(part.into());
                let result = tokio::time::timeout_at(
                    deadline,
                    tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, put_part),
                )
                .await;
                match result {
                    Ok(Ok(Ok(()))) => {}
                    Ok(Ok(Err(error))) => {
                        abort_upload(&mut upload, deadline).await;
                        return Err(storage_error(error));
                    }
                    Ok(Err(_)) | Err(_) => {
                        abort_upload(&mut upload, deadline).await;
                        return Err(transfer_timeout());
                    }
                }
            }
        }
    }

    if let Some(mut upload) = upload {
        if !buffer.is_empty() {
            let put_part = upload.put_part(buffer.freeze().into());
            match tokio::time::timeout_at(
                deadline,
                tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, put_part),
            )
            .await
            {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    let _ = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, upload.abort()).await;
                    return Err(storage_error(error));
                }
                Ok(Err(_)) | Err(_) => {
                    let _ = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, upload.abort()).await;
                    return Err(transfer_timeout());
                }
            }
        }
        match tokio::time::timeout_at(
            deadline,
            tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, upload.complete()),
        )
        .await
        {
            Ok(Ok(Ok(_))) => {}
            Ok(Ok(Err(error))) => {
                let _ = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, upload.abort()).await;
                return Err(storage_error(error));
            }
            Ok(Err(_)) | Err(_) => {
                let _ = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, upload.abort()).await;
                return Err(transfer_timeout());
            }
        }
    } else {
        tokio::time::timeout_at(
            deadline,
            tokio::time::timeout(
                TRANSFER_IDLE_TIMEOUT,
                store.put(path, buffer.freeze().into()),
            ),
        )
        .await
        .map_err(|_| transfer_timeout())?
        .map_err(|_| transfer_timeout())?
        .map_err(storage_error)?;
    }
    Ok(())
}

async fn abort_upload(
    upload: &mut Option<Box<dyn MultipartUpload>>,
    deadline: tokio::time::Instant,
) {
    if let Some(mut upload) = upload.take() {
        let _ = tokio::time::timeout_at(
            deadline,
            tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, upload.abort()),
        )
        .await;
    }
}

fn invalid_assignment() -> ApiError {
    ApiError::with_code(
        StatusCode::BAD_REQUEST,
        "invalid_object_assignment",
        "object assignment is invalid",
    )
}

fn parse_generation(value: &str) -> Result<i64, ApiError> {
    value.parse::<i64>().map_err(|_| invalid_assignment())
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
