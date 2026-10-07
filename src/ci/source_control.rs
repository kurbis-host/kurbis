use std::{collections::HashMap, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::domain::{CiRefKind, validate_commit_sha};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRefKind {
    PullRequest,
    Branch,
}
impl From<SourceRefKind> for CiRefKind {
    fn from(value: SourceRefKind) -> Self {
        match value {
            SourceRefKind::PullRequest => Self::PullRequest,
            SourceRefKind::Branch => Self::Branch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub kind: SourceRefKind,
    pub name: String,
    pub number: Option<u64>,
    pub commit_sha: String,
    pub source_repository: Option<String>,
    pub closed: bool,
    pub deleted: bool,
    pub updated_at: Option<DateTime<Utc>>,
}
impl SourceRef {
    pub fn validate(&self) -> Result<(), PollError> {
        validate_commit_sha(&self.commit_sha)
            .map_err(|e| PollError::InvalidResponse(e.to_string()))?;
        if self.name.trim().is_empty()
            || self.name.len() > 255
            || self
                .name
                .bytes()
                .any(|byte| byte == 0 || byte.is_ascii_control())
            || (self.closed && self.deleted)
        {
            return Err(PollError::InvalidResponse(
                "source ref name is empty or has contradictory lifecycle flags".into(),
            ));
        }
        if let Some(repository) = self.source_repository.as_deref() {
            parse_repository(repository)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollCheckpoint {
    /// ETag for the pull-request collection. The branch collection has its own ETag.
    pub etag: Option<String>,
    pub branch_etag: Option<String>,
    pub pull_request_page: u32,
    pub branch_cursor: Option<String>,
    pub last_successful_poll: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollResult {
    pub refs: Vec<SourceRef>,
    /// Pull-request heads whose lifecycle changed since the previous poll.
    /// Branches and arbitrary commit aliases are intentionally excluded: they
    /// remain lazy until an explicit on-demand join.
    pub eager_refs: Vec<SourceRef>,
    pub checkpoint: PollCheckpoint,
    pub not_modified: bool,
    pub retry_after: Option<Duration>,
    pub complete_kinds: Vec<SourceRefKind>,
}

#[derive(Debug, Error)]
pub enum PollError {
    #[error("source-control request failed with status {status}: {message}")]
    Http { status: StatusCode, message: String },
    #[error("source-control rate limit exceeded; retry after {retry_after:?}")]
    RateLimited { retry_after: Option<Duration> },
    #[error("source-control response was invalid: {0}")]
    InvalidResponse(String),
    #[error("source-control transport failed: {0}")]
    Transport(#[from] reqwest::Error),
}

#[async_trait]
pub trait SourceControlClient: Send + Sync {
    async fn poll(
        &self,
        repository: &str,
        checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, PollError>;
    async fn fetch_commit(&self, repository: &str, commit_sha: &str) -> Result<Vec<u8>, PollError>;
    async fn fetch_commit_from(
        &self,
        source_repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, PollError> {
        self.fetch_commit(source_repository, commit_sha).await
    }

    /// Return every commit currently reachable from an open PR's head, in
    /// GitHub's oldest-first order. The head itself is always included by the
    /// caller so simple non-GitHub fixtures may use this default.
    async fn pull_request_commits(
        &self,
        _repository: &str,
        _number: u64,
    ) -> Result<Vec<String>, PollError> {
        Ok(Vec::new())
    }

    /// Resolve the exact PR commit range captured by a signed webhook. Unlike
    /// looking up the mutable PR endpoint later, these two SHAs still identify
    /// the event's original range after the PR is force-pushed again.
    async fn commit_range(
        &self,
        _source_repository: &str,
        _base_sha: &str,
        head_sha: &str,
    ) -> Result<Vec<String>, PollError> {
        validate_commit_sha(head_sha)
            .map_err(|error| PollError::InvalidResponse(error.to_string()))?;
        Ok(vec![head_sha.to_owned()])
    }
}

#[derive(Clone)]
pub struct GitHubSourceControl {
    client: Client,
    api_base_url: String,
    token: String,
    per_page: u32,
    max_pages: u32,
}
impl GitHubSourceControl {
    pub fn new(client: Client, token: impl Into<String>) -> Result<Self, PollError> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(PollError::InvalidResponse(
                "GitHub PAT cannot be empty".into(),
            ));
        }
        Ok(Self {
            client,
            api_base_url: "https://api.github.com".into(),
            token,
            per_page: 100,
            max_pages: 100,
        })
    }
    pub fn with_api_base_url(mut self, value: impl Into<String>) -> Self {
        self.api_base_url = value.into().trim_end_matches('/').into();
        self
    }
    pub fn with_page_limits(mut self, per_page: u32, max_pages: u32) -> Self {
        self.per_page = per_page.clamp(1, 100);
        self.max_pages = max_pages.max(1);
        self
    }
    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{}", self.api_base_url, path))
            .header(header::ACCEPT, "application/vnd.github+json")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::USER_AGENT, "kurbis-ci-preview")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }
    async fn decode_error(response: reqwest::Response) -> PollError {
        let status = response.status();
        let retry_after = retry_after(&response);
        let exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            == Some("0");
        if status == StatusCode::TOO_MANY_REQUESTS || retry_after.is_some() || exhausted {
            return PollError::RateLimited { retry_after };
        }
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "GitHub request failed".into());
        PollError::Http { status, message }
    }
    async fn collect_page<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Page<T>, PollError> {
        let request = self.request(path);
        let request = match etag {
            Some(etag) => request.header(header::IF_NONE_MATCH, etag),
            None => request,
        };
        let response = request.send().await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(Page {
                items: Vec::new(),
                etag: etag.map(str::to_owned),
                not_modified: true,
            });
        }
        if !response.status().is_success() {
            return Err(Self::decode_error(response).await);
        }
        let response_etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let items = response
            .json()
            .await
            .map_err(|e| PollError::InvalidResponse(e.to_string()))?;
        Ok(Page {
            items,
            etag: response_etag,
            not_modified: false,
        })
    }
}

#[derive(Debug)]
struct Page<T> {
    items: Vec<T>,
    etag: Option<String>,
    not_modified: bool,
}
#[derive(Debug, Deserialize)]
struct PullRequest {
    number: u64,
    state: String,
    head: PullRequestHead,
    updated_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Deserialize)]
struct PullRequestHead {
    sha: String,
    repository: Option<PullRequestRepository>,
}
#[derive(Debug, Deserialize)]
struct PullRequestRepository {
    full_name: String,
}
#[derive(Debug, Deserialize)]
struct Branch {
    name: String,
    commit: BranchCommit,
}
#[derive(Debug, Deserialize)]
struct BranchCommit {
    sha: String,
}

impl GitHubSourceControl {
    async fn collect_commit_pages(
        &self,
        path: impl Fn(u32) -> String,
    ) -> Result<Vec<String>, PollError> {
        let mut commits = Vec::new();
        for page in 1..=self.max_pages {
            let result: Page<CommitSha> = self.collect_page(&path(page), None).await?;
            let count = result.items.len();
            append_valid_commit_shas(&mut commits, result.items)?;
            if count < self.per_page as usize {
                return Ok(commits);
            }
        }
        Err(PollError::InvalidResponse(format!(
            "commit enumeration exceeded the configured {}-page safety bound",
            self.max_pages
        )))
    }

    async fn collect_compare_commit_pages(
        &self,
        path: impl Fn(u32) -> String,
    ) -> Result<Vec<String>, PollError> {
        let mut commits = Vec::new();
        for page in 1..=self.max_pages {
            // GitHub's compare endpoint wraps commits in an object, unlike the
            // pull-request commit endpoint which returns a bare array.
            let response = self.request(&path(page)).send().await?;
            if !response.status().is_success() {
                return Err(Self::decode_error(response).await);
            }
            let result: ComparePage = response
                .json()
                .await
                .map_err(|error| PollError::InvalidResponse(error.to_string()))?;
            let count = result.commits.len();
            append_valid_commit_shas(&mut commits, result.commits)?;
            if count < self.per_page as usize {
                return Ok(commits);
            }
        }
        Err(PollError::InvalidResponse(format!(
            "commit comparison exceeded the configured {}-page safety bound",
            self.max_pages
        )))
    }
}

#[derive(Debug, Deserialize)]
struct CommitSha {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct ComparePage {
    commits: Vec<CommitSha>,
}

fn append_valid_commit_shas(
    commits: &mut Vec<String>,
    page: impl IntoIterator<Item = CommitSha>,
) -> Result<(), PollError> {
    for commit in page {
        validate_commit_sha(&commit.sha)
            .map_err(|error| PollError::InvalidResponse(error.to_string()))?;
        commits.push(commit.sha);
    }
    Ok(())
}

#[async_trait]
impl SourceControlClient for GitHubSourceControl {
    async fn poll(
        &self,
        repository: &str,
        checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, PollError> {
        let (owner, name) = parse_repository(repository)?;
        let previous_poll = checkpoint.last_successful_poll;
        let mut refs = Vec::new();
        let mut eager_refs = Vec::new();
        let mut pr_etag = checkpoint.etag.clone();
        let mut pr_page = 1u32;
        let mut pr_complete = false;
        for page in 1..=self.max_pages {
            let path = format!(
                "/repos/{owner}/{name}/pulls?state=all&sort=updated&direction=desc&per_page={}&page={page}",
                self.per_page
            );
            let mut result: Page<PullRequest> = self
                .collect_page(
                    &path,
                    if page == 1 {
                        checkpoint.etag.as_deref()
                    } else {
                        None
                    },
                )
                .await?;
            if result.not_modified && page == 1 {
                // A collection ETag covers this page, not later pages. Re-fetch
                // the full collection unconditionally before reconciling refs;
                // otherwise a changed/deleted later page would be invisible.
                result = self.collect_page(&path, None).await?;
            }
            if page == 1 {
                pr_etag = result.etag;
            }
            if result.items.is_empty() {
                pr_complete = true;
                break;
            }
            let count = result.items.len();
            for pull in result.items {
                let source = SourceRef {
                    kind: SourceRefKind::PullRequest,
                    name: format!("pr-{}", pull.number),
                    number: Some(pull.number),
                    commit_sha: pull.head.sha,
                    source_repository: pull.head.repository.map(|repository| repository.full_name),
                    closed: pull.state != "open",
                    deleted: false,
                    updated_at: pull.updated_at,
                };
                source.validate()?;
                let eager = previous_poll.is_none()
                    || source
                        .updated_at
                        .is_some_and(|updated_at| Some(updated_at) > previous_poll);
                if eager {
                    eager_refs.push(source.clone());
                }
                refs.push(source);
            }
            pr_page = page;
            if count < self.per_page as usize {
                pr_complete = true;
                break;
            }
        }
        let mut branch_etag = checkpoint.branch_etag.clone();
        let mut branch_page = 1u32;
        let mut branches_complete = false;
        for page in 1..=self.max_pages {
            let path = format!(
                "/repos/{owner}/{name}/branches?per_page={}&page={page}",
                self.per_page
            );
            let mut result: Page<Branch> = self
                .collect_page(
                    &path,
                    if page == 1 {
                        checkpoint.branch_etag.as_deref()
                    } else {
                        None
                    },
                )
                .await?;
            if result.not_modified && page == 1 {
                result = self.collect_page(&path, None).await?;
            }
            if page == 1 {
                branch_etag = result.etag;
            }
            if result.items.is_empty() {
                branches_complete = true;
                break;
            }
            let count = result.items.len();
            for branch in result.items {
                let source = SourceRef {
                    kind: SourceRefKind::Branch,
                    name: branch.name,
                    number: None,
                    commit_sha: branch.commit.sha,
                    source_repository: Some(repository.to_owned()),
                    closed: false,
                    deleted: false,
                    updated_at: None,
                };
                source.validate()?;
                refs.push(source);
            }
            branch_page = page;
            if count < self.per_page as usize {
                branches_complete = true;
                break;
            }
        }
        Ok(PollResult {
            refs: deduplicate_refs(refs),
            eager_refs: deduplicate_refs(eager_refs),
            checkpoint: PollCheckpoint {
                etag: pr_etag,
                branch_etag,
                pull_request_page: pr_page,
                branch_cursor: Some(branch_page.to_string()),
                last_successful_poll: Some(Utc::now()),
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: [
                pr_complete.then_some(SourceRefKind::PullRequest),
                branches_complete.then_some(SourceRefKind::Branch),
            ]
            .into_iter()
            .flatten()
            .collect(),
        })
    }
    async fn fetch_commit(&self, repository: &str, commit_sha: &str) -> Result<Vec<u8>, PollError> {
        validate_commit_sha(commit_sha).map_err(|e| PollError::InvalidResponse(e.to_string()))?;
        let (owner, name) = parse_repository(repository)?;
        let response = self
            .request(&format!("/repos/{owner}/{name}/tarball/{commit_sha}"))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Self::decode_error(response).await);
        }
        Ok(response.bytes().await?.to_vec())
    }

    async fn pull_request_commits(
        &self,
        repository: &str,
        number: u64,
    ) -> Result<Vec<String>, PollError> {
        if number == 0 {
            return Err(PollError::InvalidResponse(
                "pull request number must be positive".into(),
            ));
        }
        let (owner, name) = parse_repository(repository)?;
        self.collect_commit_pages(|page| {
            format!(
                "/repos/{owner}/{name}/pulls/{number}/commits?per_page={}&page={page}",
                self.per_page
            )
        })
        .await
    }

    async fn commit_range(
        &self,
        source_repository: &str,
        base_sha: &str,
        head_sha: &str,
    ) -> Result<Vec<String>, PollError> {
        validate_commit_sha(base_sha)
            .and_then(|_| validate_commit_sha(head_sha))
            .map_err(|error| PollError::InvalidResponse(error.to_string()))?;
        let (owner, name) = parse_repository(source_repository)?;
        self.collect_compare_commit_pages(|page| {
            format!(
                "/repos/{owner}/{name}/compare/{base_sha}...{head_sha}?per_page={}&page={page}",
                self.per_page
            )
        })
        .await
    }
}

fn parse_repository(value: &str) -> Result<(&str, &str), PollError> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return Err(PollError::InvalidResponse(
            "repository must use owner/name form".into(),
        ));
    }
    Ok((owner, name))
}
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    if let Some(delay) = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Some(Duration::from_secs(delay));
    }
    let reset_at = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())?;
    Some(Duration::from_secs(
        reset_at.saturating_sub(Utc::now().timestamp().max(0) as u64),
    ))
}
pub fn retry_delay(error: &PollError, attempt: u32) -> Duration {
    if let PollError::RateLimited {
        retry_after: Some(delay),
    } = error
    {
        return *delay;
    }
    Duration::from_secs(5 * 2u64.saturating_pow(attempt.min(6)))
}
pub fn deduplicate_refs(refs: impl IntoIterator<Item = SourceRef>) -> Vec<SourceRef> {
    let mut by_key = HashMap::new();
    for source in refs {
        by_key.insert((source.kind as u8, source.name.clone()), source);
    }
    let mut result: Vec<_> = by_key.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_backoff_is_deterministic() {
        let error = PollError::Http {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: String::new(),
        };
        assert_eq!(retry_delay(&error, 0), Duration::from_secs(5));
        assert_eq!(retry_delay(&error, 7), Duration::from_secs(320));
    }
    #[test]
    fn source_refs_reject_invalid_values() {
        assert!(parse_repository("kurbis-host/Pumpkin").is_ok());
        assert!(parse_repository("Pumpkin").is_err());
        assert!(
            SourceRef {
                kind: SourceRefKind::Branch,
                name: "main".into(),
                number: None,
                commit_sha: "not-a-sha".into(),
                source_repository: None,
                closed: false,
                deleted: false,
                updated_at: None
            }
            .validate()
            .is_err()
        );
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::any,
    };
    use serde_json::json;
    use tokio::net::TcpListener;

    async fn github_fixture(request: Request<Body>) -> axum::response::Response {
        let path = request
            .uri()
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or_default();
        let is_pr = path.contains("/pulls?");
        let page = path
            .rsplit("page=")
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(1);
        if request
            .headers()
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            == Some(if is_pr { "pr-etag" } else { "branch-etag" })
        {
            return axum::response::Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .body(Body::empty())
                .unwrap();
        }
        let body = if is_pr {
            if page == 1 {
                json!([{"number": 123, "state": "open", "head": {"sha": "0123456789abcdef0123456789abcdef01234567", "repository": {"full_name": "fork/Pumpkin"}}}])
            } else {
                json!([])
            }
        } else if page == 1 {
            json!([{"name": "main", "commit": {"sha": "fedcba9876543210fedcba9876543210fedcba98"}}])
        } else {
            json!([])
        };
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::ETAG, if is_pr { "pr-etag" } else { "branch-etag" })
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[derive(Debug, Deserialize)]
    struct LiveBranchFixture {
        pulls: Vec<serde_json::Value>,
        branches: Vec<serde_json::Value>,
    }

    async fn github_branch_fixture(request: Request<Body>) -> axum::response::Response {
        let path = request
            .uri()
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or_default();
        let is_pr = path.contains("/pulls?");
        let fixture: LiveBranchFixture = serde_json::from_str(include_str!(
            "../../verification/fixtures/github_polling_branch.json"
        ))
        .unwrap();
        let etag = if is_pr {
            "live-pr-etag"
        } else {
            "live-branch-etag"
        };
        if request
            .headers()
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            == Some(etag)
        {
            return axum::response::Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .body(Body::empty())
                .unwrap();
        }
        let body = if is_pr {
            serde_json::to_vec(&fixture.pulls).unwrap()
        } else {
            serde_json::to_vec(&fixture.branches).unwrap()
        };
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::ETAG, etag)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    async fn github_commit_range_fixture(request: Request<Body>) -> axum::response::Response {
        let path = request
            .uri()
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or_default();
        let page = path
            .rsplit("page=")
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(1);
        let body = if path.contains("/pulls/314/commits") {
            match page {
                1 => json!([{"sha":"0123456789abcdef0123456789abcdef01234567"}]),
                _ => json!([]),
            }
        } else if path.contains("/compare/") {
            match page {
                1 => json!({"commits":[{"sha":"fedcba9876543210fedcba9876543210fedcba98"}]}),
                _ => json!({"commits":[]}),
            }
        } else {
            return axum::response::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        };
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn github_commit_enumeration_handles_array_and_compare_object_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(any(github_commit_range_fixture)),
            )
            .await
            .unwrap();
        });
        let client = GitHubSourceControl::new(Client::new(), "fixture-token")
            .unwrap()
            .with_api_base_url(format!("http://{address}"))
            .with_page_limits(1, 4);
        assert_eq!(
            client
                .pull_request_commits("kurbis-host/Pumpkin", 314)
                .await
                .unwrap(),
            vec!["0123456789abcdef0123456789abcdef01234567"]
        );
        assert_eq!(
            client
                .commit_range(
                    "kurbis-host/Pumpkin",
                    "0123456789abcdef0123456789abcdef01234567",
                    "fedcba9876543210fedcba9876543210fedcba98",
                )
                .await
                .unwrap(),
            vec!["fedcba9876543210fedcba9876543210fedcba98"]
        );
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn github_live_branch_fixture_never_marks_branch_heads_eager() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(any(github_branch_fixture)))
                .await
                .unwrap();
        });
        let client = GitHubSourceControl::new(Client::new(), "fixture-token")
            .unwrap()
            .with_api_base_url(format!("http://{address}"));
        let first = client
            .poll("kurbis-host/Pumpkin", &PollCheckpoint::default())
            .await
            .unwrap();
        assert_eq!(first.refs.len(), 2);
        assert!(first.eager_refs.is_empty());
        assert!(first.refs.iter().any(|source| {
            source.kind == SourceRefKind::Branch
                && source.name == "copilot/pumpkin-game-tick-research"
        }));
        let unchanged = client
            .poll("kurbis-host/Pumpkin", &first.checkpoint)
            .await
            .unwrap();
        assert!(unchanged.eager_refs.is_empty());
        assert!(
            unchanged
                .refs
                .iter()
                .all(|source| source.kind == SourceRefKind::Branch)
        );
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn github_polling_handles_pagination_and_collection_etags() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(any(github_fixture)))
                .await
                .unwrap();
        });
        let client = GitHubSourceControl::new(Client::new(), "test-token")
            .unwrap()
            .with_api_base_url(format!("http://{address}"))
            .with_page_limits(1, 4);
        let result = client
            .poll("kurbis-host/Pumpkin", &PollCheckpoint::default())
            .await
            .unwrap();
        assert_eq!(result.refs.len(), 2);
        assert!(result.refs.iter().any(|value| value.name == "pr-123"));
        assert_eq!(
            result.eager_refs.len(),
            1,
            "initial PR observation is eager"
        );
        assert_eq!(result.checkpoint.etag.as_deref(), Some("pr-etag"));
        assert_eq!(
            result.checkpoint.branch_etag.as_deref(),
            Some("branch-etag")
        );
        let unchanged = client
            .poll("kurbis-host/Pumpkin", &result.checkpoint)
            .await
            .unwrap();
        assert!(!unchanged.not_modified);
        assert!(
            unchanged.eager_refs.is_empty(),
            "unchanged PR heads stay cached"
        );
        assert_eq!(unchanged.refs.len(), 2);
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}
