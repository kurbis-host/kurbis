use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, Utc};
use rand::random;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::OAuthConfig;

use super::api::{ApiError, AppState};

const SESSION_COOKIE: &str = "kurbis_session";
const SESSION_DAYS: i64 = 30;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: Option<String>,
    pub display_name: String,
    pub account_status: String,
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallback {
    code: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

pub async fn login(State(state): State<Arc<AppState>>) -> Result<Redirect, ApiError> {
    let raw_state = random_token();
    sqlx::query("INSERT INTO oauth_states(state_hash, expires_at) VALUES($1, now() + interval '10 minutes')")
        .bind(token_hash(&raw_state))
        .execute(&state.pool)
        .await?;

    let mut url =
        Url::parse(&state.config.oauth.authorize_url).context("invalid OAUTH_AUTHORIZE_URL")?;
    url.query_pairs_mut()
        .append_pair("client_id", &state.config.oauth.client_id)
        .append_pair("redirect_uri", &callback_url(&state))
        .append_pair("response_type", "code")
        .append_pair("scope", &state.config.oauth.scopes)
        .append_pair("state", &raw_state);
    Ok(Redirect::temporary(url.as_str()))
}

pub async fn callback(
    State(state): State<Arc<AppState>>,
    Query(params): Query<OAuthCallback>,
) -> Result<Response, ApiError> {
    let valid: Option<Vec<u8>> = sqlx::query_scalar(
        "DELETE FROM oauth_states WHERE state_hash = $1 AND expires_at > now() RETURNING state_hash",
    )
    .bind(token_hash(&params.state))
    .fetch_optional(&state.pool)
    .await?;
    if valid.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid or expired OAuth state",
        ));
    }

    let callback = callback_url(&state);
    let token: TokenResponse = state
        .http
        .post(&state.config.oauth.token_url)
        .header(header::ACCEPT, "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", params.code.as_str()),
            ("redirect_uri", callback.as_str()),
            ("client_id", state.config.oauth.client_id.as_str()),
            ("client_secret", state.config.oauth.client_secret.as_str()),
        ])
        .send()
        .await
        .context("OAuth token request failed")?
        .error_for_status()
        .context("OAuth token exchange was rejected")?
        .json()
        .await
        .context("invalid OAuth token response")?;

    let profile: serde_json::Value = state
        .http
        .get(&state.config.oauth.userinfo_url)
        .bearer_auth(&token.access_token)
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .context("OAuth userinfo request failed")?
        .error_for_status()
        .context("OAuth userinfo request was rejected")?
        .json()
        .await
        .context("invalid OAuth userinfo response")?;
    let subject = profile
        .get("sub")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .or_else(|| {
            profile
                .get("id")
                .map(|v| v.to_string().trim_matches('"').to_owned())
        })
        .context("OAuth profile has neither sub nor id")?;
    let email = profile
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let display_name = profile
        .get("name")
        .or_else(|| profile.get("login"))
        .and_then(|v| v.as_str())
        .unwrap_or(&subject)
        .to_owned();

    let user_id = upsert_user(
        &state.pool,
        &state.config.oauth,
        &subject,
        email.as_deref(),
        &display_name,
    )
    .await?;
    let session = random_token();
    sqlx::query("INSERT INTO sessions(token_hash, user_id, expires_at) VALUES($1, $2, $3)")
        .bind(token_hash(&session))
        .bind(user_id)
        .bind(Utc::now() + Duration::days(SESSION_DAYS))
        .execute(&state.pool)
        .await?;

    let mut response = Redirect::to(&state.config.panel_origin).into_response();
    let cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age={}",
        SESSION_DAYS * 24 * 60 * 60
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie.parse().expect("generated cookie is valid"),
    );
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
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0")
            .parse()
            .expect("static cookie is valid"),
    );
    Ok(response)
}

pub async fn authenticate(pool: &PgPool, headers: &HeaderMap) -> Result<CurrentUser> {
    let token = session_token(headers).context("missing session cookie")?;
    let user = sqlx::query_as::<_, CurrentUser>(
        "SELECT u.id, u.email, u.display_name, u.account_status
         FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE s.token_hash = $1 AND s.expires_at > now()",
    )
    .bind(token_hash(token))
    .fetch_optional(pool)
    .await?
    .context("invalid or expired session")?;
    if user.account_status != "active" {
        bail!("account is suspended");
    }
    Ok(user)
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|cookie| {
        let (name, value) = cookie.trim().split_once('=')?;
        (name == SESSION_COOKIE).then_some(value)
    })
}

async fn upsert_user(
    pool: &PgPool,
    oauth: &OAuthConfig,
    subject: &str,
    email: Option<&str>,
    display_name: &str,
) -> Result<Uuid> {
    Ok(sqlx::query_scalar(
        "INSERT INTO users(id, oauth_provider, oauth_subject, email, display_name)
         VALUES($1,$2,$3,$4,$5)
         ON CONFLICT(oauth_provider, oauth_subject) DO UPDATE
           SET email = EXCLUDED.email, display_name = EXCLUDED.display_name, updated_at = now()
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(&oauth.provider)
    .bind(subject)
    .bind(email)
    .bind(display_name)
    .fetch_one(pool)
    .await?)
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random::<[u8; 32]>())
}

fn token_hash(token: &str) -> Vec<u8> {
    blake3::hash(token.as_bytes()).as_bytes().to_vec()
}

fn callback_url(state: &AppState) -> String {
    format!(
        "{}/auth/callback",
        state.config.public_base_url.trim_end_matches('/')
    )
}
