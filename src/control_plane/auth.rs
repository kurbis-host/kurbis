use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{config::OAuthProvider, pumpkin_control::VerifiedMinecraftIdentity};

use super::{
    api::{ApiError, AppState},
    identity::{self, IdentityError},
};

const SESSION_COOKIE: &str = "kurbis_session";
const OAUTH_COOKIE_PREFIX: &str = "kurbis_oauth_state_";
const SESSION_COOKIE_DAYS: i64 = 30;
const MINECRAFT_TOKEN_MINUTES: i64 = 15;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: Option<String>,
    pub display_name: String,
    pub account_status: String,
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallback {
    code: Option<String>,
    state: String,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, sqlx::FromRow)]
struct OAuthState {
    intent: String,
    user_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinecraftLinkRequest {
    identity: VerifiedMinecraftIdentity,
    minecraft_player_id: Uuid,
    display_name: String,
}

#[derive(Debug, Serialize)]
pub struct MinecraftLinkResponse {
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MinecraftRedeemRequest {
    token: String,
}

#[derive(Debug, sqlx::FromRow)]
struct MinecraftLoginToken {
    provider: String,
    subject: String,
    minecraft_player_id: Uuid,
    display_name: String,
}

pub async fn login(
    Path(provider): Path<OAuthProvider>,
    State(state): State<Arc<AppState>>,
) -> Result<Response, ApiError> {
    begin_oauth(&state, provider, None).await
}

pub async fn link_oauth(
    Path(provider): Path<OAuthProvider>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let user = authenticate(&state.pool, &headers).await.map_err(|_| {
        ApiError::with_code(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "authentication required",
        )
    })?;
    require_active(&user)?;
    begin_oauth(&state, provider, Some(user.id)).await
}

async fn begin_oauth(
    state: &AppState,
    provider: OAuthProvider,
    link_to: Option<Uuid>,
) -> Result<Response, ApiError> {
    let config = state.config.oauth.provider(provider);
    let raw_state = random_token();
    sqlx::query("DELETE FROM oauth_states WHERE expires_at <= now()")
        .execute(&state.pool)
        .await?;
    sqlx::query(
        "INSERT INTO oauth_states(state_hash, provider, intent, user_id, expires_at)
         VALUES($1, $2, $3, $4, now() + interval '10 minutes')",
    )
    .bind(token_hash(&raw_state))
    .bind(provider.as_str())
    .bind(if link_to.is_some() { "link" } else { "login" })
    .bind(link_to)
    .execute(&state.pool)
    .await?;

    let mut url = Url::parse(&config.authorize_url).context("invalid OAuth authorize URL")?;
    url.query_pairs_mut()
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &callback_url(state, provider))
        .append_pair("response_type", "code")
        .append_pair("scope", &config.scopes)
        .append_pair("state", &raw_state);
    let mut response = Redirect::temporary(url.as_str()).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "{}{provider_name}={raw_state}; Path=/auth/{provider_name}; HttpOnly; Secure; SameSite=Lax; Max-Age=600",
            OAUTH_COOKIE_PREFIX,
            provider_name = provider.as_str(),
        )
        .parse()
        .expect("generated cookie is valid"),
    );
    Ok(response)
}

pub async fn callback(
    Path(provider): Path<OAuthProvider>,
    State(state): State<Arc<AppState>>,
    Query(params): Query<OAuthCallback>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let oauth_cookie = oauth_cookie_name(provider);
    if cookie_value(&headers, &oauth_cookie) != Some(params.state.as_str()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "OAuth state does not match this browser",
        ));
    }
    let valid: Option<OAuthState> = sqlx::query_as(
        "DELETE FROM oauth_states
         WHERE state_hash = $1 AND provider = $2 AND expires_at > now()
         RETURNING intent, user_id",
    )
    .bind(token_hash(&params.state))
    .bind(provider.as_str())
    .fetch_optional(&state.pool)
    .await?;
    let oauth_state = valid
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "invalid or expired OAuth state"))?;

    if params.error.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "OAuth sign-in was cancelled",
        ));
    }
    let code = params.code.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "OAuth response is missing its code",
        )
    })?;
    let config = state.config.oauth.provider(provider);
    let callback = callback_url(&state, provider);
    let token: TokenResponse = state
        .http
        .post(&config.token_url)
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", callback.as_str()),
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
        ])
        .send()
        .await
        .map_err(|error| oauth_request_error(error, "OAuth token request failed"))?
        .error_for_status()
        .map_err(|error| oauth_request_error(error, "OAuth token exchange was rejected"))?
        .json()
        .await
        .map_err(|error| oauth_request_error(error, "invalid OAuth token response"))?;

    let profile: serde_json::Value = state
        .http
        .get(&config.userinfo_url)
        .bearer_auth(&token.access_token)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| oauth_request_error(error, "OAuth userinfo request failed"))?
        .error_for_status()
        .map_err(|error| oauth_request_error(error, "OAuth userinfo request was rejected"))?
        .json()
        .await
        .map_err(|error| oauth_request_error(error, "invalid OAuth userinfo response"))?;
    let subject = profile
        .get("sub")
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .or_else(|| {
            profile
                .get("id")
                .map(|value| value.to_string().trim_matches('"').to_owned())
        })
        .filter(|subject| !subject.is_empty())
        .ok_or_else(|| oauth_profile_error("OAuth profile has no user identifier"))?;
    let email = profile.get("email").and_then(|value| value.as_str());
    let email_verified = email.is_some()
        && match provider {
            OAuthProvider::Google => {
                profile
                    .get("email_verified")
                    .and_then(|value| value.as_bool())
                    == Some(true)
            }
            OAuthProvider::Discord => {
                profile.get("verified").and_then(|value| value.as_bool()) == Some(true)
            }
        };
    let display_name = profile
        .get("name")
        .or_else(|| profile.get("global_name"))
        .or_else(|| profile.get("username"))
        .or_else(|| profile.get("login"))
        .and_then(|value| value.as_str())
        .unwrap_or(&subject)
        .to_owned();

    let link_to = if oauth_state.intent == "link" {
        let initiating_user = oauth_state.user_id.ok_or_else(|| {
            ApiError::with_code(
                StatusCode::BAD_REQUEST,
                "invalid_oauth_flow",
                "invalid account linking flow",
            )
        })?;
        let current_user = authenticate(&state.pool, &headers).await.map_err(|_| {
            ApiError::with_code(
                StatusCode::UNAUTHORIZED,
                "authentication_required",
                "sign in again before linking an identity",
            )
        })?;
        if current_user.id != initiating_user {
            return Err(ApiError::with_code(
                StatusCode::CONFLICT,
                "account_changed",
                "the active account changed while linking",
            ));
        }
        require_active(&current_user)?;
        Some(initiating_user)
    } else {
        None
    };

    let user_id = identity::link_oauth_identity(
        &state.pool,
        provider.as_str(),
        &subject,
        email,
        email_verified,
        &display_name,
        link_to,
    )
    .await
    .map_err(map_identity_error)?;

    let mut response = if link_to.is_some() {
        Redirect::to(&format!(
            "{}/app/account",
            state.config.public_base_url.trim_end_matches('/')
        ))
        .into_response()
    } else {
        let session = create_session(&state.pool, user_id).await?;
        let mut response = Redirect::to(&format!(
            "{}/app",
            state.config.public_base_url.trim_end_matches('/')
        ))
        .into_response();
        set_session_cookie(&mut response, &session);
        response
    };
    response.headers_mut().append(
        header::SET_COOKIE,
        expired_oauth_cookie(provider)
            .parse()
            .expect("static cookie is valid"),
    );
    Ok(response)
}

pub async fn create_minecraft_link(
    State(state): State<Arc<AppState>>,
    Json(request): Json<MinecraftLinkRequest>,
) -> Result<Json<MinecraftLinkResponse>, ApiError> {
    let MinecraftLinkRequest {
        identity,
        minecraft_player_id,
        display_name,
    } = request;
    let key = identity::minecraft_identity_key(&identity, minecraft_player_id)
        .map_err(|message| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, message))?;
    if display_name.trim().is_empty()
        || display_name.len() > 64
        || display_name.chars().any(char::is_control)
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "verified Minecraft profile is invalid",
        ));
    }

    let token = random_token();
    sqlx::query("DELETE FROM minecraft_login_tokens WHERE expires_at <= now()")
        .execute(&state.pool)
        .await?;
    sqlx::query(
        "INSERT INTO minecraft_login_tokens(token_hash, provider, subject, minecraft_player_id, display_name, expires_at)
         VALUES($1, $2, $3, $4, $5, now() + make_interval(mins => $6))",
    )
    .bind(token_hash(&token))
    .bind(key.provider)
    .bind(key.subject)
    .bind(minecraft_player_id)
    .bind(display_name.trim())
    .bind(MINECRAFT_TOKEN_MINUTES as i32)
    .execute(&state.pool)
    .await?;
    Ok(Json(MinecraftLinkResponse {
        url: format!(
            "{}/auth/minecraft#token={token}",
            state.config.public_base_url.trim_end_matches('/')
        ),
    }))
}

pub(super) async fn redeem_minecraft(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<MinecraftRedeemRequest>,
) -> Result<Response, ApiError> {
    if !same_origin(&headers, &state.config.public_base_url) {
        return Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "cross_origin_request",
            "Minecraft login redemption must come from this site",
        ));
    }
    if request.token.len() != 43
        || !request
            .token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid login token",
        ));
    }

    let browser_user = if session_token(&headers).is_some() {
        let user = authenticate(&state.pool, &headers).await.map_err(|_| {
            ApiError::with_code(
                StatusCode::UNAUTHORIZED,
                "invalid_session",
                "the current browser session is no longer valid",
            )
        })?;
        require_active(&user)?;
        Some(user.id)
    } else {
        None
    };
    let token: Option<MinecraftLoginToken> = sqlx::query_as(
        "DELETE FROM minecraft_login_tokens
         WHERE token_hash = $1 AND expires_at > now()
         RETURNING provider, subject, minecraft_player_id, display_name",
    )
    .bind(token_hash(&request.token))
    .fetch_optional(&state.pool)
    .await?;
    let token = token.ok_or_else(|| {
        ApiError::with_code(
            StatusCode::UNAUTHORIZED,
            "invalid_login_token",
            "login link is invalid, expired, or already used",
        )
    })?;
    let user_id = identity::attach_minecraft_identity(
        &state.pool,
        &token.provider,
        &token.subject,
        token.minecraft_player_id,
        &token.display_name,
        browser_user,
    )
    .await
    .map_err(map_identity_error)?;

    let mut response = StatusCode::NO_CONTENT.into_response();
    if browser_user.is_none() {
        let session = create_session(&state.pool, user_id).await?;
        set_session_cookie(&mut response, &session);
    }
    Ok(response)
}

pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = session_token(&headers) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash(token))
            .execute(&state.pool)
            .await?;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        expired_session_cookie()
            .parse()
            .expect("static cookie is valid"),
    );
    Ok(response)
}

pub async fn authenticate(pool: &PgPool, headers: &HeaderMap) -> Result<CurrentUser> {
    let token = session_token(headers).context("missing session cookie")?;
    sqlx::query_as::<_, CurrentUser>(
        "SELECT u.id,
                (SELECT i.email FROM identities i
                 WHERE i.user_id = u.id AND i.email_verified
                 ORDER BY i.created_at, i.id LIMIT 1) AS email,
                u.display_name, u.account_status
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1",
    )
    .bind(token_hash(token))
    .fetch_optional(pool)
    .await?
    .context("invalid session")
}

pub async fn renew_session_cookie(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let session = session_token(request.headers()).map(str::to_owned);
    let mut response = next.run(request).await;
    if let Some(session) = session {
        let active: Option<Uuid> = sqlx::query_scalar(
            "UPDATE sessions SET last_seen_at = now() WHERE token_hash = $1 RETURNING id",
        )
        .bind(token_hash(&session))
        .fetch_optional(&state.pool)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(?error, "failed to refresh session cookie lifetime");
            None
        });
        if active.is_some() {
            response.headers_mut().append(
                header::SET_COOKIE,
                session_cookie(&session)
                    .parse()
                    .expect("generated cookie is valid"),
            );
        }
    }
    response
}

pub fn token_hash(token: &str) -> Vec<u8> {
    blake3::hash(token.as_bytes()).as_bytes().to_vec()
}

pub fn current_session_hash(headers: &HeaderMap) -> Option<Vec<u8>> {
    session_token(headers).map(token_hash)
}

fn create_session_cookie_value(token: &str) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={}",
        SESSION_COOKIE_DAYS * 24 * 60 * 60
    )
}

async fn create_session(pool: &PgPool, user_id: Uuid) -> Result<String, ApiError> {
    let session = random_token();
    sqlx::query("INSERT INTO sessions(id, token_hash, user_id) VALUES($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(token_hash(&session))
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(session)
}

fn set_session_cookie(response: &mut Response, token: &str) {
    response.headers_mut().append(
        header::SET_COOKIE,
        session_cookie(token)
            .parse()
            .expect("generated cookie is valid"),
    );
}

fn session_cookie(token: &str) -> String {
    create_session_cookie_value(token)
}

fn expired_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0")
}

fn expired_oauth_cookie(provider: OAuthProvider) -> String {
    format!(
        "{}={}; Path=/auth/{}; HttpOnly; Secure; SameSite=Lax; Max-Age=0",
        oauth_cookie_name(provider),
        "",
        provider.as_str()
    )
}

fn oauth_cookie_name(provider: OAuthProvider) -> String {
    format!(
        "{}{provider}",
        OAUTH_COOKIE_PREFIX,
        provider = provider.as_str()
    )
}

fn require_active(user: &CurrentUser) -> Result<(), ApiError> {
    if user.account_status == "active" {
        Ok(())
    } else {
        Err(ApiError::with_code(
            StatusCode::FORBIDDEN,
            "account_suspended",
            "account is suspended",
        ))
    }
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    cookie_value(headers, SESSION_COOKIE)
}

pub(super) fn cookie_value<'a>(headers: &'a HeaderMap, cookie_name: &str) -> Option<&'a str> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|cookie| {
        let (name, value) = cookie.trim().split_once('=')?;
        (name == cookie_name).then_some(value)
    })
}

pub(super) fn same_origin(headers: &HeaderMap, public_base_url: &str) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(expected) = Url::parse(public_base_url) else {
        return false;
    };
    let Ok(actual) = Url::parse(origin) else {
        return false;
    };
    actual.origin() == expected.origin() && actual.path() == "/" && actual.query().is_none()
}

fn map_identity_error(error: IdentityError) -> ApiError {
    match error {
        IdentityError::AlreadyLinked => ApiError::with_code(
            StatusCode::CONFLICT,
            "identity_already_linked",
            "that provider identity belongs to another account",
        ),
        IdentityError::AccountMissing => ApiError::with_code(
            StatusCode::CONFLICT,
            "account_changed",
            "the account changed while this identity was being linked",
        ),
        IdentityError::Database(error) => error.into(),
    }
}

pub(super) fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random::<[u8; 32]>())
}

fn callback_url(state: &AppState, provider: OAuthProvider) -> String {
    format!(
        "{}/auth/{}/callback",
        state.config.public_base_url.trim_end_matches('/'),
        provider.as_str(),
    )
}

fn oauth_request_error(error: reqwest::Error, event: &'static str) -> ApiError {
    tracing::warn!(
        ?error,
        provider_request = event,
        "OAuth provider request failed"
    );
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "OAuth sign-in could not be completed",
    )
}

fn oauth_profile_error(reason: &'static str) -> ApiError {
    tracing::warn!(reason, "OAuth provider returned an invalid profile");
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "OAuth sign-in could not be completed",
    )
}
