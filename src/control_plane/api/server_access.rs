use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use super::{ApiError, AppState, auth, worker_auth};

const LOGIN_TOKEN_TTL_SECONDS: i64 = 15 * 60;
const SERVER_SESSION_TTL_SECONDS: i64 = 60 * 60;
const SERVER_SESSION_COOKIE: &str = "kurbis_server_session";
const MAX_LINKS_PER_MINUTE: i32 = 5;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CapabilityRequest {
    server_id: Uuid,
    generation: i64,
    current_capability: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct CapabilityResponse {
    capability: String,
}

pub(super) async fn ensure_server_capability(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CapabilityRequest>,
) -> Result<Response, ApiError> {
    let authenticated_worker_id = worker_auth::authenticated_worker_id(&state, &headers).await?;
    if request.generation < 1
        || request
            .current_capability
            .as_deref()
            .is_some_and(|value| !valid_token(value))
    {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            "invalid_capability_request",
            "invalid server capability request",
        ));
    }

    let mut tx = state.pool.begin().await?;
    let placement = sqlx::query(
        "SELECT p.worker_id, p.generation AS placement_generation,
                s.generation AS server_generation, s.desired_state::text
         FROM placements p JOIN servers s ON s.id = p.server_id
         WHERE p.server_id = $1 FOR UPDATE OF p, s",
    )
    .bind(request.server_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(placement) = placement else {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "stale_placement",
            "server placement is no longer current",
        ));
    };
    let placement_worker_id: Uuid = placement.try_get("worker_id")?;
    let placement_generation: i64 = placement.try_get("placement_generation")?;
    let server_generation: i64 = placement.try_get("server_generation")?;
    let desired_state: String = placement.try_get("desired_state")?;
    if placement_worker_id != authenticated_worker_id
        || placement_generation != request.generation
        || server_generation != request.generation
        || desired_state == "deleted"
    {
        return Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "stale_placement",
            "server placement is no longer current",
        ));
    }

    let current = sqlx::query(
        "SELECT worker_id, placement_generation, token_hash FROM server_instance_capabilities
         WHERE server_id = $1 FOR UPDATE",
    )
    .bind(request.server_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let (Some(current), Some(raw_capability)) =
        (current.as_ref(), request.current_capability.as_deref())
    {
        let stored_worker_id: Uuid = current.try_get("worker_id")?;
        let stored_generation: i64 = current.try_get("placement_generation")?;
        let stored_hash: Vec<u8> = current.try_get("token_hash")?;
        if stored_worker_id == authenticated_worker_id
            && stored_generation == request.generation
            && stored_hash == auth::token_hash(raw_capability)
        {
            tx.commit().await?;
            return Ok(no_store(Json(CapabilityResponse {
                capability: raw_capability.to_owned(),
            })));
        }
    }

    let capability = random_token();
    sqlx::query(
        "INSERT INTO server_instance_capabilities(server_id, worker_id, placement_generation, token_hash)
         VALUES($1, $2, $3, $4)
         ON CONFLICT(server_id) DO UPDATE
         SET worker_id = EXCLUDED.worker_id,
             placement_generation = EXCLUDED.placement_generation,
             token_hash = EXCLUDED.token_hash, updated_at = now()",
    )
    .bind(request.server_id)
    .bind(authenticated_worker_id)
    .bind(request.generation)
    .bind(auth::token_hash(&capability))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(no_store(Json(CapabilityResponse { capability })))
}

#[derive(Debug, Serialize)]
pub(super) struct LoginLinkResponse {
    url: String,
    expires_in_seconds: i64,
}

pub(super) async fn mint_server_login_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let capability = bearer_token(&headers).filter(|value| valid_token(value));
    let Some(capability) = capability else {
        return Err(invalid_capability());
    };
    let mut tx = state.pool.begin().await?;
    let current = sqlx::query(
        "SELECT c.server_id
         FROM server_instance_capabilities c
         JOIN servers s ON s.id = c.server_id
         JOIN placements p ON p.server_id = c.server_id AND p.worker_id = c.worker_id
         JOIN users u ON u.id = s.owner_id AND u.account_status = 'active'
         WHERE c.token_hash = $1 AND c.placement_generation = p.generation
           AND s.generation = p.generation AND p.active
           AND s.desired_state <> 'deleted'
         FOR UPDATE OF c, s, p",
    )
    .bind(auth::token_hash(capability))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current) = current else {
        return Err(invalid_capability());
    };
    let server_id: Uuid = current.try_get("server_id")?;
    let count: i32 = sqlx::query_scalar(
        "INSERT INTO server_login_rate_limits(server_id, window_started_at, request_count)
         VALUES($1, now(), 1)
         ON CONFLICT(server_id) DO UPDATE SET
           request_count = CASE
             WHEN server_login_rate_limits.window_started_at <= now() - interval '1 minute' THEN 1
             ELSE LEAST(server_login_rate_limits.request_count + 1, $2)
           END,
           window_started_at = CASE
             WHEN server_login_rate_limits.window_started_at <= now() - interval '1 minute' THEN now()
             ELSE server_login_rate_limits.window_started_at
           END
         RETURNING request_count",
    )
    .bind(server_id)
    .bind(MAX_LINKS_PER_MINUTE + 1)
    .fetch_one(&mut *tx)
    .await?;
    if count > MAX_LINKS_PER_MINUTE {
        tx.commit().await?;
        return Err(ApiError::with_code(
            StatusCode::TOO_MANY_REQUESTS,
            "login_link_rate_limited",
            "too many login links requested for this server; try again shortly",
        ));
    }

    sqlx::query("DELETE FROM server_login_tokens WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    let token = random_token();
    sqlx::query(
        "INSERT INTO server_login_tokens(token_hash, server_id, expires_at)
         VALUES($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(auth::token_hash(&token))
    .bind(server_id)
    .bind(LOGIN_TOKEN_TTL_SECONDS as f64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(no_store(Json(LoginLinkResponse {
        url: format!(
            "{}/auth/server#token={token}",
            state.config.public_base_url.trim_end_matches('/')
        ),
        expires_in_seconds: LOGIN_TOKEN_TTL_SECONDS,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ServerRedeemRequest {
    token: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ServerRedeemResponse {
    server_id: Uuid,
}

pub(super) async fn redeem_server_login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<ServerRedeemRequest>,
) -> Result<Response, ApiError> {
    if !auth::same_origin(&headers, &state.config.public_base_url) {
        return Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "cross_origin_request",
            "server login redemption must come from this site",
        ));
    }
    if !valid_token(&request.token) {
        return Err(ApiError::with_code(
            StatusCode::BAD_REQUEST,
            "invalid_login_token",
            "login link is invalid, expired, or already used",
        ));
    }

    let mut tx = state.pool.begin().await?;
    let server_id: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM server_login_tokens
         WHERE token_hash = $1 AND expires_at > now()
         RETURNING server_id",
    )
    .bind(auth::token_hash(&request.token))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(server_id) = server_id else {
        return Err(ApiError::with_code(
            StatusCode::UNAUTHORIZED,
            "invalid_login_token",
            "login link is invalid, expired, or already used",
        ));
    };
    sqlx::query("DELETE FROM server_sessions WHERE expires_at <= now()")
        .execute(&mut *tx)
        .await?;
    let session = random_token();
    sqlx::query(
        "INSERT INTO server_sessions(token_hash, server_id, expires_at)
         VALUES($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(auth::token_hash(&session))
    .bind(server_id)
    .bind(SERVER_SESSION_TTL_SECONDS as f64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut response = no_store(Json(ServerRedeemResponse { server_id }));
    response.headers_mut().append(
        header::SET_COOKIE,
        format!(
            "{SERVER_SESSION_COOKIE}={session}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={SERVER_SESSION_TTL_SECONDS}"
        )
        .parse()
        .expect("generated server session cookie is valid"),
    );
    Ok(response)
}

fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) async fn server_session_server_id(
    pool: &sqlx::PgPool,
    headers: &HeaderMap,
) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(token) = cookie_value(headers, SERVER_SESSION_COOKIE) else {
        return Ok(None);
    };
    if !valid_token(token) {
        return Ok(None);
    }
    sqlx::query_scalar(
        "SELECT ss.server_id FROM server_sessions ss
         JOIN servers s ON s.id = ss.server_id AND s.desired_state <> 'deleted'
         JOIN users u ON u.id = s.owner_id AND u.account_status = 'active'
         WHERE ss.token_hash = $1 AND ss.expires_at > now()",
    )
    .bind(auth::token_hash(token))
    .fetch_optional(pool)
    .await
}

pub(super) async fn instance_capability_server_id(
    pool: &sqlx::PgPool,
    headers: &HeaderMap,
) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(capability) = bearer_token(headers).filter(|value| valid_token(value)) else {
        return Ok(None);
    };
    sqlx::query_scalar(
        "SELECT c.server_id
         FROM server_instance_capabilities c
         JOIN placements p ON p.server_id = c.server_id
                           AND p.worker_id = c.worker_id
                           AND p.generation = c.placement_generation AND p.active
         JOIN servers s ON s.id = c.server_id AND s.generation = p.generation
         JOIN users u ON u.id = s.owner_id AND u.account_status = 'active'
         WHERE c.token_hash = $1 AND s.desired_state <> 'deleted'",
    )
    .bind(auth::token_hash(capability))
    .fetch_optional(pool)
    .await
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

fn valid_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then_some(value)
        })
}

fn invalid_capability() -> ApiError {
    ApiError::with_code(
        StatusCode::UNAUTHORIZED,
        "invalid_server_capability",
        "server capability is invalid or stale",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_credentials_are_exact_url_safe_tokens() {
        let token = random_token();
        assert!(valid_token(&token));
        assert!(!valid_token("not-a-token"));
        assert!(!valid_token(&format!("{token}x")));
        let mut invalid = token.clone();
        invalid.replace_range(..1, ".");
        assert!(!valid_token(&invalid));
    }
}
