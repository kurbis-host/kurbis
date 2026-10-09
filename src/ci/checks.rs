use async_trait::async_trait;
use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use thiserror::Error;

use super::rate_limit::{GitHubRateLimitConfig, GitHubRateLimitGuard};

use super::domain::{CiBuildStage, validate_commit_sha};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
    Queued,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunConclusion {
    Success,
    Failure,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckRunDetails {
    pub commit_sha: String,
    pub stage: CiBuildStage,
    pub repository: String,
    pub artifact_digest: Option<String>,
    pub failure_stage: Option<CiBuildStage>,
    pub failure_message: Option<String>,
    pub build_log: String,
    pub preview_hostnames: Vec<String>,
    pub expires_at: Option<String>,
    pub details_url: Option<String>,
}
impl CheckRunDetails {
    pub fn validate(&self) -> Result<(), CheckRunError> {
        validate_commit_sha(&self.commit_sha).map_err(|e| CheckRunError::Invalid(e.to_string()))?;
        if self.repository.split('/').count() != 2 || self.preview_hostnames.len() > 8 {
            return Err(CheckRunError::Invalid(
                "invalid repository or hostname count".into(),
            ));
        }
        if let Some(details_url) = self.details_url.as_deref() {
            let url = reqwest::Url::parse(details_url).map_err(|error| {
                CheckRunError::Invalid(format!("invalid Check Run details URL: {error}"))
            })?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(CheckRunError::Invalid(
                    "Check Run details URL must be an HTTPS link without credentials or fragments"
                        .into(),
                ));
            }
        }
        if self
            .failure_message
            .as_ref()
            .is_some_and(|message| message.len() > 8 * 1024)
            || self.build_log.len() > 8 * 1024
        {
            return Err(CheckRunError::Invalid(
                "Check Run diagnostics exceed the 8 KiB limit".into(),
            ));
        }
        Ok(())
    }
    pub fn summary(&self) -> String {
        match self.failure_stage {
            Some(stage) => format!("Pumpkin artifact build failed during {stage}"),
            None => format!("Pumpkin artifact build is {}", self.stage),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckRunId(pub i64);

#[derive(Debug, Error)]
pub enum CheckRunError {
    #[error("invalid check-run input: {0}")]
    Invalid(String),
    #[error("GitHub check-run request failed with status {status}: {message}")]
    Http { status: StatusCode, message: String },
    #[error("GitHub check-run rate limit exceeded; retry after {retry_after:?}")]
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    #[error("GitHub transport failed: {0}")]
    Transport(#[from] reqwest::Error),
}

#[async_trait]
pub trait CheckRunClient: Send + Sync {
    async fn create(
        &self,
        repository: &str,
        details: &CheckRunDetails,
    ) -> Result<CheckRunId, CheckRunError>;
    async fn update(
        &self,
        repository: &str,
        id: CheckRunId,
        details: &CheckRunDetails,
    ) -> Result<(), CheckRunError>;
    async fn get_or_create(
        &self,
        repository: &str,
        details: &CheckRunDetails,
    ) -> Result<CheckRunId, CheckRunError> {
        self.create(repository, details).await
    }
}

#[derive(Clone)]
pub struct GitHubCheckRunClient {
    client: Client,
    api_base_url: String,
    details_base_url: Option<String>,
    token: String,
    rate_limit: Arc<GitHubRateLimitGuard>,
}
impl GitHubCheckRunClient {
    pub fn new(client: Client, token: impl Into<String>) -> Result<Self, CheckRunError> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(CheckRunError::Invalid("GitHub PAT cannot be empty".into()));
        }
        Ok(Self {
            client,
            api_base_url: "https://api.github.com".into(),
            details_base_url: None,
            token,
            rate_limit: Arc::new(GitHubRateLimitGuard::new(GitHubRateLimitConfig::default())),
        })
    }
    pub fn with_details_base_url(
        mut self,
        value: impl Into<String>,
    ) -> Result<Self, CheckRunError> {
        let value = value.into();
        let url = reqwest::Url::parse(&value).map_err(|error| {
            CheckRunError::Invalid(format!("invalid Check Run details URL: {error}"))
        })?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CheckRunError::Invalid(
                "Check Run details base must be an HTTPS URL without credentials, query, or fragment".into(),
            ));
        }
        self.details_base_url = Some(value.trim_end_matches('/').to_owned());
        Ok(self)
    }

    pub fn with_api_base_url(mut self, value: impl Into<String>) -> Self {
        self.api_base_url = value.into().trim_end_matches('/').into();
        self
    }
    pub fn with_rate_limit_guard(mut self, guard: Arc<GitHubRateLimitGuard>) -> Self {
        self.rate_limit = guard;
        self
    }
    pub fn with_rate_limit_store(mut self, pool: sqlx::PgPool) -> Self {
        self.rate_limit = Arc::new((*self.rate_limit).clone().with_store(pool));
        self
    }
    fn payload(&self, details: &CheckRunDetails) -> CheckRunPayload {
        let mut payload = payload(details);
        if payload.details_url.is_none()
            && let Some(base) = self.details_base_url.as_deref()
            && let Some(hostname) = details
                .preview_hostnames
                .iter()
                .find(|hostname| hostname.starts_with("commit-"))
        {
            payload.details_url = Some(format!("{base}/{hostname}"));
        }
        payload
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.api_base_url, path))
            .header(header::ACCEPT, "application/vnd.github+json")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::USER_AGENT, "kurbis-ci-preview")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }
    async fn execute_json(
        &self,
        method: reqwest::Method,
        path: &str,
        payload: Option<serde_json::Value>,
    ) -> Result<Vec<u8>, CheckRunError> {
        let scope = if path.contains("/check-runs") {
            "github:checks:permission"
        } else {
            "github:rest:core"
        };
        self.rate_limit
            .before_request(scope)
            .await
            .map_err(|error| match error {
                crate::ci::PollError::RateLimited { retry_after } => {
                    CheckRunError::RateLimited { retry_after }
                }
                other => CheckRunError::Http {
                    status: StatusCode::TOO_MANY_REQUESTS,
                    message: other.to_string(),
                },
            })?;
        let mut request = self.request(method, path);
        if let Some(payload) = payload {
            request = request.json(&payload);
        }
        let response = request.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await?.to_vec();
        // Rate-limit headers belong to GitHub's shared core quota even when
        // this feature has its own permission circuit breaker.
        if let Some(delay) = self
            .rate_limit
            .observe_response("github:rest:core", status, &headers, &body)
            .await
        {
            return Err(CheckRunError::RateLimited {
                retry_after: Some(delay),
            });
        }
        if !status.is_success() {
            let message = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(|message| message.as_str())
                        .map(str::to_owned)
                })
                .or_else(|| String::from_utf8(body).ok())
                .unwrap_or_else(|| "GitHub request failed".into());
            if status == StatusCode::FORBIDDEN {
                // GitHub returns a plain 403 when the PAT is valid but lacks
                // Checks write access. Keep this separate from core-quota
                // state and suppress retries for one bounded lease.
                self.rate_limit
                    .pause_for("github:checks:permission", Duration::from_secs(300))
                    .await;
            }
            return Err(CheckRunError::Http { status, message });
        }
        Ok(body)
    }
    fn repo(value: &str) -> Result<(&str, &str), CheckRunError> {
        let mut parts = value.split('/');
        let owner = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if owner.is_empty() || name.is_empty() || parts.next().is_some() {
            return Err(CheckRunError::Invalid(
                "repository must use owner/name".into(),
            ));
        };
        Ok((owner, name))
    }
}
#[derive(Debug, Deserialize)]
struct CreatedCheckRun {
    id: i64,
}
#[derive(Debug, Deserialize)]
struct ExistingCheckRun {
    id: i64,
    name: String,
    head_sha: String,
}
#[derive(Debug, Deserialize)]
struct ExistingCheckRuns {
    check_runs: Vec<ExistingCheckRun>,
}
#[derive(Debug, Serialize)]
struct CheckRunPayload {
    name: String,
    head_sha: String,
    status: CheckRunStatus,
    conclusion: Option<CheckRunConclusion>,
    details_url: Option<String>,
    output: CheckRunOutput,
}
#[derive(Debug, Serialize)]
struct CheckRunOutput {
    title: String,
    summary: String,
    text: Option<String>,
}
fn payload(details: &CheckRunDetails) -> CheckRunPayload {
    let status = if details.stage == CiBuildStage::Queued {
        CheckRunStatus::Queued
    } else if details.stage.is_terminal() {
        CheckRunStatus::Completed
    } else {
        CheckRunStatus::InProgress
    };
    let conclusion = details.stage.github_conclusion().map(|value| match value {
        "success" => CheckRunConclusion::Success,
        "failure" => CheckRunConclusion::Failure,
        _ => CheckRunConclusion::Cancelled,
    });
    let summary = details.summary();
    let mut text = vec![
        format!("Repository: {}", details.repository),
        format!("Commit: {}", details.commit_sha),
    ];
    if let Some(digest) = details.artifact_digest.as_deref() {
        text.push(format!("Artifact digest: {digest}"));
    }
    if !details.preview_hostnames.is_empty() {
        text.push(format!(
            "Preview hostnames: {}",
            details.preview_hostnames.join(", ")
        ));
    }
    if let Some(expires_at) = details.expires_at.as_deref() {
        text.push(format!("Artifact cache expires: {expires_at}"));
    }
    if let Some(failure) = details.failure_message.as_deref() {
        text.push(format!("Failure details: {}", escape_markdown(failure)));
    }
    if !details.build_log.is_empty() {
        text.push(format!(
            "Build log:\n{}",
            escape_markdown(&details.build_log)
        ));
    }
    CheckRunPayload {
        name: "Pumpkin CI build".into(),
        head_sha: details.commit_sha.clone(),
        status,
        conclusion,
        details_url: details.details_url.clone(),
        output: CheckRunOutput {
            title: "Pumpkin CI build".into(),
            summary,
            text: Some(text.join("\n")),
        },
    }
}
fn escape_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len().min(8 * 1024));
    for character in value.chars().take(8 * 1024) {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '<'
                | '>'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
        ) {
            escaped.push('\\');
        }
        if character.is_control() && !matches!(character, '\n' | '\t') {
            escaped.push(' ');
        } else {
            escaped.push(character);
        }
    }
    escaped
}

#[async_trait]
impl CheckRunClient for GitHubCheckRunClient {
    async fn create(
        &self,
        repository: &str,
        details: &CheckRunDetails,
    ) -> Result<CheckRunId, CheckRunError> {
        details.validate()?;
        let (owner, name) = Self::repo(repository)?;
        let body = self
            .execute_json(
                reqwest::Method::POST,
                &format!("/repos/{owner}/{name}/check-runs"),
                Some(
                    serde_json::to_value(self.payload(details))
                        .map_err(|e| CheckRunError::Invalid(e.to_string()))?,
                ),
            )
            .await?;
        Ok(CheckRunId(
            serde_json::from_slice::<CreatedCheckRun>(&body)
                .map_err(|e| CheckRunError::Invalid(e.to_string()))?
                .id,
        ))
    }
    async fn get_or_create(
        &self,
        repository: &str,
        details: &CheckRunDetails,
    ) -> Result<CheckRunId, CheckRunError> {
        details.validate()?;
        let (owner, name) = Self::repo(repository)?;
        let body = self
            .execute_json(
                reqwest::Method::GET,
                &format!("/repos/{owner}/{name}/commits/{}/check-runs?check_name=Pumpkin%20CI%20build&per_page=100", details.commit_sha),
                None,
            )
            .await?;
        let existing = serde_json::from_slice::<ExistingCheckRuns>(&body)
            .map_err(|error| CheckRunError::Invalid(error.to_string()))?;
        if let Some(check) = existing
            .check_runs
            .into_iter()
            .filter(|check| {
                check.name == "Pumpkin CI build"
                    && check.head_sha.eq_ignore_ascii_case(&details.commit_sha)
            })
            .max_by_key(|check| check.id)
        {
            return Ok(CheckRunId(check.id));
        }
        self.create(repository, details).await
    }

    async fn update(
        &self,
        repository: &str,
        id: CheckRunId,
        details: &CheckRunDetails,
    ) -> Result<(), CheckRunError> {
        details.validate()?;
        let (owner, name) = Self::repo(repository)?;
        self.execute_json(
            reqwest::Method::PATCH,
            &format!("/repos/{owner}/{name}/check-runs/{}", id.0),
            Some(
                serde_json::to_value(self.payload(details))
                    .map_err(|e| CheckRunError::Invalid(e.to_string()))?,
            ),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn check_run_details_link_uses_only_the_public_commit_status_page() {
        let client = GitHubCheckRunClient::new(Client::new(), "fixture-token")
            .unwrap()
            .with_details_base_url("https://panel.example.test/ci/previews")
            .unwrap();
        let details = CheckRunDetails {
            commit_sha: "a".repeat(40),
            stage: CiBuildStage::Queued,
            repository: "Pumpkin-MC/Pumpkin".into(),
            artifact_digest: None,
            failure_stage: None,
            failure_message: None,
            build_log: String::new(),
            preview_hostnames: vec![
                "commit-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.pumpkin.kurbis.host".into(),
                "branch-feature-123456789abc.pumpkin.kurbis.host".into(),
            ],
            expires_at: None,
            details_url: None,
        };
        let payload = client.payload(&details);
        assert_eq!(
            payload.details_url.as_deref(),
            Some(
                "https://panel.example.test/ci/previews/commit-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.pumpkin.kurbis.host"
            )
        );
        assert!(
            client
                .clone()
                .with_details_base_url("http://panel.example.test")
                .is_err()
        );
    }

    #[test]
    fn check_run_escapes_untrusted_build_diagnostics_as_plain_text() {
        let details = CheckRunDetails {
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            stage: CiBuildStage::Failed,
            repository: "Pumpkin-MC/Pumpkin".into(),
            artifact_digest: None,
            failure_stage: Some(CiBuildStage::Compiling),
            failure_message: Some("[click](https://evil.example)".into()),
            build_log: "![x](https://evil.example)".into(),
            preview_hostnames: Vec::new(),
            expires_at: None,
            details_url: None,
        };
        let payload = payload(&details);
        assert!(!payload.output.summary.contains("https://evil.example"));
        let text = payload.output.text.unwrap();
        assert!(text.contains("\\[click\\]\\(https://evil\\.example\\)"));
        assert!(text.contains("\\!\\[x\\]\\(https://evil\\.example\\)"));
    }

    #[test]
    fn check_run_is_build_only_and_maps_terminal_stages() {
        let details = CheckRunDetails {
            commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            stage: CiBuildStage::Ready,
            repository: "Pumpkin-MC/Pumpkin".into(),
            artifact_digest: Some("sha256:abc".into()),
            failure_stage: None,
            failure_message: None,
            build_log: String::new(),
            preview_hostnames: vec![],
            expires_at: None,
            details_url: None,
        };
        let p = payload(&details);
        assert_eq!(p.status, CheckRunStatus::Completed);
        assert_eq!(p.conclusion, Some(CheckRunConclusion::Success));
    }
}
