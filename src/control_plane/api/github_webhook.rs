use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    ci::{CiGitHubPrDelivery, CiGitHubPrDeliveryResult, CiPreviewRepository, validate_commit_sha},
    control_plane::api::{ApiError, AppState},
};

const MAX_GITHUB_WEBHOOK_BYTES: usize = 1024 * 1024;
const TARGET_REPOSITORY: &str = "kurbis-host/Pumpkin";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Deserialize)]
struct PullRequestEvent {
    action: String,
    repository: GitHubRepository,
    pull_request: PullRequestPayload,
}

#[derive(Debug, Deserialize)]
struct GitHubRepository {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestPayload {
    number: u64,
    base: PullRequestBase,
    head: PullRequestHead,
}

#[derive(Debug, Deserialize)]
struct PullRequestBase {
    sha: String,
    repo: GitHubRepository,
}

#[derive(Debug, Deserialize)]
struct PullRequestHead {
    sha: String,
    repo: Option<GitHubRepository>,
}

pub(super) async fn ingest(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let Some(secret) = state.config.ci_github_webhook_secret.as_ref() else {
        return Err(ApiError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "github_webhook_not_configured",
            "GitHub webhook ingestion is not configured",
        ));
    };
    let body = to_bytes(body, MAX_GITHUB_WEBHOOK_BYTES)
        .await
        .map_err(|_| bad_request("webhook payload exceeds the size limit"))?;
    verify_signature(&headers, secret.expose(), &body)?;

    let delivery_id = required_header(&headers, "x-github-delivery")?
        .parse::<Uuid>()
        .map_err(|_| bad_request("invalid webhook delivery identifier"))?;
    let event_name = required_header(&headers, "x-github-event")?;
    if event_name != "pull_request" {
        return Ok(StatusCode::NO_CONTENT);
    }
    let event: PullRequestEvent = serde_json::from_slice(&body)
        .map_err(|_| bad_request("invalid pull request webhook payload"))?;
    if !matches!(event.action.as_str(), "opened" | "reopened" | "synchronize") {
        return Ok(StatusCode::NO_CONTENT);
    }
    if event.repository.full_name != TARGET_REPOSITORY
        || event.pull_request.base.repo.full_name != TARGET_REPOSITORY
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    if event.pull_request.number == 0 {
        return Err(bad_request("pull request number must be positive"));
    }
    // GitHub can null the fork after it is deleted. Preserve the event as a
    // failing build attempt instead of silently losing its commit SHA.
    let source_repository = event
        .pull_request
        .head
        .repo
        .map(|repository| repository.full_name)
        .unwrap_or_else(|| TARGET_REPOSITORY.to_owned());
    validate_repository(&source_repository)
        .map_err(|_| bad_request("invalid pull request source repository"))?;
    validate_commit_sha(&event.pull_request.base.sha)
        .and_then(|_| validate_commit_sha(&event.pull_request.head.sha))
        .map_err(|_| bad_request("invalid pull request commit SHA"))?;

    let delivery = CiGitHubPrDelivery {
        delivery_id,
        repository: TARGET_REPOSITORY.to_owned(),
        action: event.action,
        pull_request_number: event.pull_request.number,
        source_repository,
        base_sha: event.pull_request.base.sha,
        head_sha: event.pull_request.head.sha,
        payload_digest: blake3::hash(&body).as_bytes().to_vec(),
    };
    let repository = CiPreviewRepository::new(state.pool.clone());
    match repository.record_github_pr_delivery(&delivery).await {
        Ok(CiGitHubPrDeliveryResult::Inserted | CiGitHubPrDeliveryResult::Duplicate) => {
            Ok(StatusCode::ACCEPTED)
        }
        Ok(CiGitHubPrDeliveryResult::AtCapacity) => Err(ApiError::with_code(
            StatusCode::TOO_MANY_REQUESTS,
            "github_webhook_queue_full",
            "GitHub webhook queue is temporarily full",
        )),
        Ok(CiGitHubPrDeliveryResult::ConflictingReplay) => Err(ApiError::with_code(
            StatusCode::CONFLICT,
            "github_webhook_delivery_conflict",
            "webhook delivery identifier was reused with different content",
        )),
        Err(error) => Err(ApiError::from(error)),
    }
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn verify_signature(headers: &HeaderMap, secret: &str, body: &[u8]) -> Result<(), ApiError> {
    let signature = required_header(headers, "x-hub-signature-256")?;
    let hex = signature
        .strip_prefix("sha256=")
        .filter(|value| value.len() == 64)
        .ok_or_else(unauthorized)?;
    let mut expected = [0u8; 32];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).ok_or_else(unauthorized)?;
        let low = hex_nibble(pair[1]).ok_or_else(unauthorized)?;
        expected[index] = (high << 4) | low;
    }
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| unauthorized())?;
    mac.update(body);
    mac.verify_slice(&expected).map_err(|_| unauthorized())
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn required_header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, ApiError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| bad_request("required webhook header is missing"))
}

fn validate_repository(value: &str) -> Result<(), ()> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || parts.next().is_some()
        || value.len() > 255
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(());
    }
    Ok(())
}

fn unauthorized() -> ApiError {
    ApiError::with_code(
        StatusCode::UNAUTHORIZED,
        "invalid_github_signature",
        "webhook signature is invalid",
    )
}

fn bad_request(message: &'static str) -> ApiError {
    ApiError::with_code(StatusCode::BAD_REQUEST, "invalid_github_webhook", message)
}
