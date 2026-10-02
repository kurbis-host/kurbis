use std::time::Duration;

use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode},
};
use bytes::{Bytes, BytesMut};
use futures::{StreamExt, stream};
use object_store::{MultipartUpload, ObjectStore, path::Path as ObjectPath};
use sqlx::{PgPool, Postgres, pool::PoolConnection};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::authorization::{Assignment, validate_key_authority_locked};
use super::{
    ApiError, AppState, object_too_large, storage_error, storage_error_message, transfer_timeout,
};
use crate::{model::SERVER_STORAGE_BYTES, snapshot::WorkerObjectKey};

const MULTIPART_PART_BYTES: usize = 8 * 1024 * 1024;
const GET_AUTH_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const S3_SINGLE_COPY_LIMIT_BYTES: u64 = 5_000_000_000;
const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OBJECT_TRANSFER_TIME: Duration = Duration::from_secs(2 * 60 * 60);

pub(super) async fn storage_gc_guard(pool: &PgPool) -> Result<PoolConnection<Postgres>, ApiError> {
    let mut connection = pool.acquire().await?;
    connection.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock_shared(724515, 0)")
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn publish_staged_object(
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

pub(super) async fn delete_staged_object(state: &AppState, path: &ObjectPath) {
    match state.object_store.delete(path).await {
        Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
        Err(error) => tracing::warn!(
            ?error,
            "failed to clean worker object upload staging object"
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn body_with_reauthorization(
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

pub(super) async fn stream_object_body(
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
