use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Url;
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use thiserror::Error;

use crate::ci::{PollCheckpoint, PollError, PollResult, SourceControlClient};

use super::archive::{
    MAX_COMPRESSED_SOURCE_BYTES, TRUSTED_PUMPKIN_SUBMODULE_PATHS, encode_source_bundle,
};

const USER_AGENT: &str = "kurbis-ci-preview";
const TRUSTED_SUBMODULES: &[(&str, &str)] =
    &[("crates/pumpkin-plugin-wit", "Pumpkin-MC/pumpkin-plugin-wit")];
const MAX_GITHUB_API_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error)]
enum FetchArchiveError {
    #[error("source archive request failed with HTTP {0}")]
    Http(StatusCode),
    #[error("GitHub source response exceeded its size limit")]
    Limit,
    #[error("GitHub source response was invalid: {0}")]
    Invalid(String),
    #[error("GitHub source transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("GitHub source IO failed: {0}")]
    Io(#[from] std::io::Error),
}

/// GitHub polling remains delegated to the configured source-control client;
/// source retrieval resolves only the requested immutable commit and pinned submodules.
#[derive(Clone)]
pub struct GitHubExactSource<C> {
    polling: Arc<C>,
    client: Client,
    api_base_url: Url,
    token: String,
}

impl<C> GitHubExactSource<C> {
    pub fn new(polling: Arc<C>, token: impl Into<String>) -> Result<Self, PollError> {
        Self::with_api_base_url(polling, token, "https://api.github.com")
    }

    pub fn with_api_base_url(
        polling: Arc<C>,
        token: impl Into<String>,
        api_base_url: &str,
    ) -> Result<Self, PollError> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(PollError::InvalidResponse(
                "GitHub PAT cannot be empty".into(),
            ));
        }
        let api_base_url = Url::parse(api_base_url)
            .map_err(|error| PollError::InvalidResponse(error.to_string()))?;
        if !matches!(api_base_url.scheme(), "https" | "http")
            || api_base_url.host_str().is_none()
            || !api_base_url.username().is_empty()
            || api_base_url.password().is_some()
            || api_base_url.query().is_some()
            || api_base_url.fragment().is_some()
        {
            return Err(PollError::InvalidResponse(
                "GitHub API base URL is invalid".into(),
            ));
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| PollError::InvalidResponse(error.to_string()))?;
        Ok(Self {
            polling,
            client,
            api_base_url,
            token,
        })
    }

    async fn source_archive(
        &self,
        repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, FetchArchiveError> {
        let (owner, name) = parse_repository(repository)?;
        validate_sha(commit_sha)?;
        let primary = self
            .download_tarball(&format!("/repos/{owner}/{name}/tarball/{commit_sha}"))
            .await?;

        // Resolve Git submodules from the immutable commit tree, not by
        // extracting the untrusted source archive in the control-plane process.
        let tree = self.commit_tree(repository, commit_sha).await?;
        let mut submodules = Vec::new();
        for entry in tree
            .iter()
            .filter(|entry| entry.mode == "160000" && entry.kind == "commit")
        {
            let Some((_, trusted_repository)) = TRUSTED_SUBMODULES
                .iter()
                .find(|(path, _)| *path == entry.path)
            else {
                return Err(FetchArchiveError::Invalid(format!(
                    "unapproved source submodule {}",
                    entry.path
                )));
            };
            if !TRUSTED_PUMPKIN_SUBMODULE_PATHS.contains(&entry.path.as_str()) {
                return Err(FetchArchiveError::Invalid(
                    "source tree contains an unsupported Pumpkin submodule".into(),
                ));
            }
            validate_sha(&entry.sha)?;
            let archive = self
                .download_tarball(&format!(
                    "/repos/{trusted_repository}/tarball/{}",
                    entry.sha
                ))
                .await?;
            submodules.push((entry.path.clone(), archive));
        }
        if submodules.is_empty() {
            return Ok(primary);
        }
        encode_source_bundle(&primary, &submodules)
            .map_err(|error| FetchArchiveError::Invalid(error.to_string()))
    }

    async fn download_tarball(&self, path: &str) -> Result<Vec<u8>, FetchArchiveError> {
        let url = self.api_url(path)?;
        let response = self
            .client
            .get(url)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::USER_AGENT, USER_AGENT)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;
        let response = if response.status().is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    FetchArchiveError::Invalid("archive redirect has no valid location".into())
                })?;
            let redirected = self
                .api_base_url
                .join(location)
                .map_err(|error| FetchArchiveError::Invalid(error.to_string()))?;
            let host = redirected.host_str().unwrap_or_default();
            let base_host = self.api_base_url.host_str().unwrap_or_default();
            if redirected.scheme() != "https"
                || !(host == base_host || host == "codeload.github.com")
                || !redirected.username().is_empty()
                || redirected.password().is_some()
            {
                return Err(FetchArchiveError::Invalid(
                    "archive redirect left the trusted GitHub download hosts".into(),
                ));
            }
            self.client
                .get(redirected)
                .header(header::USER_AGENT, USER_AGENT)
                .send()
                .await?
        } else {
            response
        };
        if !response.status().is_success() {
            return Err(FetchArchiveError::Http(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_COMPRESSED_SOURCE_BYTES as u64)
        {
            return Err(FetchArchiveError::Limit);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > MAX_COMPRESSED_SOURCE_BYTES {
                return Err(FetchArchiveError::Limit);
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Err(FetchArchiveError::Invalid("empty source archive".into()));
        }
        Ok(bytes)
    }

    async fn commit_tree(
        &self,
        repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<TreeEntry>, FetchArchiveError> {
        let (owner, name) = parse_repository(repository)?;
        let response = self
            .client
            .get(self.api_url(&format!(
                "/repos/{owner}/{name}/git/trees/{commit_sha}?recursive=1"
            ))?)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::USER_AGENT, USER_AGENT)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(FetchArchiveError::Http(response.status()));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_GITHUB_API_RESPONSE_BYTES as u64)
        {
            return Err(FetchArchiveError::Limit);
        }
        let bytes = response.bytes().await?;
        if bytes.len() > MAX_GITHUB_API_RESPONSE_BYTES {
            return Err(FetchArchiveError::Limit);
        }
        let tree: TreeResponse = serde_json::from_slice(&bytes)
            .map_err(|error| FetchArchiveError::Invalid(error.to_string()))?;
        if tree.truncated {
            return Err(FetchArchiveError::Invalid(
                "GitHub truncated the exact-commit tree response".into(),
            ));
        }
        Ok(tree.tree)
    }

    fn api_url(&self, path: &str) -> Result<Url, FetchArchiveError> {
        let base = self.api_base_url.as_str().trim_end_matches('/');
        Url::parse(&format!("{base}{path}"))
            .map_err(|error| FetchArchiveError::Invalid(error.to_string()))
    }
}

#[async_trait]
impl<C> SourceControlClient for GitHubExactSource<C>
where
    C: SourceControlClient + 'static,
{
    async fn poll(
        &self,
        repository: &str,
        checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, PollError> {
        self.polling.poll(repository, checkpoint).await
    }

    async fn fetch_commit(&self, repository: &str, commit_sha: &str) -> Result<Vec<u8>, PollError> {
        self.source_archive(repository, commit_sha)
            .await
            .map_err(to_poll_error)
    }

    async fn fetch_commit_from(
        &self,
        source_repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, PollError> {
        self.source_archive(source_repository, commit_sha)
            .await
            .map_err(to_poll_error)
    }
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    truncated: bool,
    tree: Vec<TreeEntry>,
}
#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

fn parse_repository(repository: &str) -> Result<(&str, &str), FetchArchiveError> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 100
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            && value != "."
            && value != ".."
    };
    if !valid(owner) || !valid(name) || parts.next().is_some() {
        return Err(FetchArchiveError::Invalid(
            "repository must use safe owner/name form".into(),
        ));
    }
    Ok((owner, name))
}

fn validate_sha(sha: &str) -> Result<(), FetchArchiveError> {
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(FetchArchiveError::Invalid(
            "commit SHA is not a full SHA-1".into(),
        ));
    }
    Ok(())
}

fn to_poll_error(error: FetchArchiveError) -> PollError {
    match error {
        FetchArchiveError::Http(status) => PollError::Http {
            status,
            message: "GitHub source archive request failed".into(),
        },
        FetchArchiveError::Transport(error) => PollError::Transport(error),
        error => PollError::InvalidResponse(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router, body::Body, extract::State, http::HeaderMap, response::Response, routing::any,
    };
    use std::sync::Mutex;
    use tokio::{net::TcpListener, task::JoinHandle};

    #[derive(Clone)]
    struct EmptyPolling;
    #[async_trait]
    impl SourceControlClient for EmptyPolling {
        async fn poll(&self, _: &str, _: &PollCheckpoint) -> Result<PollResult, PollError> {
            unreachable!("source fetch tests do not poll")
        }
        async fn fetch_commit(&self, _: &str, _: &str) -> Result<Vec<u8>, PollError> {
            unreachable!("the exact-source adapter owns commit fetching")
        }
    }

    struct FixtureApi {
        primary: Vec<u8>,
        submodule: Vec<u8>,
        tree: String,
        requests: Mutex<Vec<String>>,
    }

    async fn source_fixture_handler(
        State(fixture): State<Arc<FixtureApi>>,
        headers: HeaderMap,
        request: axum::http::Request<Body>,
    ) -> Response<Body> {
        use axum::http::{StatusCode, header};
        if headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer fixture-token")
        {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Body::empty())
                .unwrap();
        }
        let path = request.uri().path().to_owned();
        fixture.requests.lock().unwrap().push(path.clone());
        if path.starts_with("/repos/kurbis-host/Pumpkin/tarball/") {
            Response::new(Body::from(fixture.primary.clone()))
        } else if path
            == "/repos/kurbis-host/Pumpkin/git/trees/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        {
            Response::new(Body::from(fixture.tree.clone()))
        } else if path
            == "/repos/Pumpkin-MC/pumpkin-plugin-wit/tarball/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        {
            Response::new(Body::from(fixture.submodule.clone()))
        } else {
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        }
    }

    async fn start_fixture(
        tree: &str,
    ) -> (
        GitHubExactSource<EmptyPolling>,
        Arc<FixtureApi>,
        JoinHandle<()>,
    ) {
        let fixture = Arc::new(FixtureApi {
            primary: b"primary archive bytes".to_vec(),
            submodule: b"trusted submodule archive bytes".to_vec(),
            tree: tree.to_owned(),
            requests: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .fallback(any(source_fixture_handler))
            .with_state(fixture.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let source = GitHubExactSource::with_api_base_url(
            Arc::new(EmptyPolling),
            "fixture-token",
            &format!("http://{address}"),
        )
        .unwrap();
        (source, fixture, server)
    }

    #[test]
    fn source_repository_and_commit_are_allowlisted_as_path_components() {
        assert!(parse_repository("kurbis-host/Pumpkin").is_ok());
        assert!(parse_repository("kurbis-host/Pumpkin?x=1").is_err());
        assert!(parse_repository("../evil/Pumpkin").is_err());
        assert!(parse_repository("kurbis-host/Pumpkin/extra").is_err());
        assert!(validate_sha(&"a".repeat(40)).is_ok());
        assert!(validate_sha("not-a-sha").is_err());
    }

    #[tokio::test]
    async fn exact_source_fetch_bundles_only_the_pinned_trusted_submodule() {
        use super::super::archive::decode_source_bundle;
        let tree = r#"{"truncated":false,"tree":[{"path":"crates/pumpkin-plugin-wit","mode":"160000","type":"commit","sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}]}"#;
        let (source, fixture, server) = start_fixture(tree).await;
        let result = source
            .fetch_commit_from("kurbis-host/Pumpkin", &"a".repeat(40))
            .await
            .unwrap();
        let bundle = decode_source_bundle(&result).unwrap();
        assert_eq!(bundle.primary, b"primary archive bytes");
        assert_eq!(
            bundle.submodules,
            vec![(
                "crates/pumpkin-plugin-wit".to_owned(),
                b"trusted submodule archive bytes".to_vec()
            )]
        );
        let requests = fixture.requests.lock().unwrap();
        assert!(requests.iter().any(|path| path
            == "/repos/kurbis-host/Pumpkin/tarball/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(requests.iter().any(|path| path
            == "/repos/kurbis-host/Pumpkin/git/trees/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(requests.iter().any(|path| path == "/repos/Pumpkin-MC/pumpkin-plugin-wit/tarball/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
        drop(requests);
        server.abort();
    }

    #[tokio::test]
    async fn exact_source_fetch_rejects_unapproved_gitlinks_before_download() {
        let tree = r#"{"truncated":false,"tree":[{"path":"thirdparty/untrusted","mode":"160000","type":"commit","sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}]}"#;
        let (source, fixture, server) = start_fixture(tree).await;
        let result = source
            .fetch_commit_from("kurbis-host/Pumpkin", &"a".repeat(40))
            .await;
        assert!(result.is_err());
        let requests = fixture.requests.lock().unwrap();
        assert!(
            !requests
                .iter()
                .any(|path| path.starts_with("/repos/Pumpkin-MC/"))
        );
        drop(requests);
        server.abort();
    }
}
