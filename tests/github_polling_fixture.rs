use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Method, Request, StatusCode, header},
    response::Response,
    routing::any,
};
use chrono::{Duration as ChronoDuration, Utc};
use kurbis_hoster::ci::{
    CheckRunClient, CheckRunDetails, CheckRunId, CiBuildStage, GitHubCheckRunClient,
    GitHubSourceControl, PollCheckpoint, SourceControlClient, SourceRefKind,
};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::net::TcpListener;

const REPOSITORY: &str = "Pumpkin-MC/Pumpkin";
const PR_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const BRANCH_SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: Method,
    path_and_query: String,
    if_none_match: Option<String>,
    authorization: Option<String>,
    body: Value,
}

#[derive(Default)]
struct PollFixture {
    requests: Mutex<Vec<RecordedRequest>>,
    recent_updated_at: String,
    stale_updated_at: String,
}

async fn github_poll_fixture(
    State(fixture): State<Arc<PollFixture>>,
    request: Request<Body>,
) -> Response {
    let path_and_query = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    let method = request.method().clone();
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map(|bytes| serde_json::from_slice(&bytes).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    let is_pull_request = path.contains("/pulls?") || path.contains("/pulls");
    let first_page = path_and_query.ends_with("page=1");
    let etag = if is_pull_request {
        "pumpkin-pr-etag-v1"
    } else {
        "pumpkin-branch-etag-v1"
    };
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    fixture.requests.lock().unwrap().push(RecordedRequest {
        method,
        path_and_query: path_and_query.clone(),
        if_none_match: if_none_match.clone(),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body,
    });

    if first_page && if_none_match.as_deref() == Some(etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .unwrap();
    }

    let body = if is_pull_request {
        if path_and_query.ends_with("page=1") {
            json!([{
                "number": 314,
                "state": "open",
                "updated_at": fixture.recent_updated_at,
                "head": {
                    "sha": PR_SHA,
                    "repository": {"full_name": "contributor/Pumpkin"}
                }
            }])
        } else if path_and_query.ends_with("page=2") {
            json!([{
                "number": 313,
                "state": "open",
                "updated_at": fixture.stale_updated_at,
                "head": {
                    "sha": BRANCH_SHA,
                    "repository": {"full_name": "older-contributor/Pumpkin"}
                }
            }])
        } else {
            json!([])
        }
    } else if path_and_query.ends_with("page=1") {
        json!([{"name": "master", "commit": {"sha": BRANCH_SHA}}])
    } else {
        json!([])
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::ETAG, etag)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn github_check_run_fixture(
    State(requests): State<Arc<Mutex<Vec<RecordedRequest>>>>,
    request: Request<Body>,
) -> Response {
    let path_and_query = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or_default()
        .to_owned();
    let method = request.method().clone();
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map(|bytes| serde_json::from_slice(&bytes).unwrap_or(Value::Null))
        .unwrap_or(Value::Null);
    requests.lock().unwrap().push(RecordedRequest {
        method: method.clone(),
        path_and_query,
        if_none_match: headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body,
    });

    let (status, body) = match method {
        Method::GET => (StatusCode::OK, json!({"total_count": 0, "check_runs": []})),
        Method::POST => (StatusCode::CREATED, json!({"id": 987654321})),
        Method::PATCH => (StatusCode::OK, json!({"id": 987654321})),
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            json!({"message":"unsupported"}),
        ),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn start_poll_server(fixture: Arc<PollFixture>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .fallback(any(github_poll_fixture))
                .with_state(fixture),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), server)
}

async fn start_check_run_server(
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .fallback(any(github_check_run_fixture))
                .with_state(requests),
        )
        .await
        .unwrap();
    });
    (format!("http://{address}"), server)
}

#[tokio::test]
async fn pumpkin_mc_poll_uses_live_repository_path_etags_pages_and_pr_only_eagerness() {
    let now = Utc::now();
    let fixture = Arc::new(PollFixture {
        requests: Mutex::new(Vec::new()),
        recent_updated_at: (now - ChronoDuration::minutes(30)).to_rfc3339(),
        stale_updated_at: (now - ChronoDuration::hours(2)).to_rfc3339(),
    });
    let (api_base, server) = start_poll_server(fixture.clone()).await;
    let client = GitHubSourceControl::new(Client::new(), "fixture-token")
        .unwrap()
        .with_api_base_url(api_base)
        .with_page_limits(1, 4);
    let prior_poll = PollCheckpoint {
        last_successful_poll: Some(now - ChronoDuration::hours(1)),
        ..PollCheckpoint::default()
    };

    let first = client.poll(REPOSITORY, &prior_poll).await.unwrap();
    assert_eq!(first.refs.len(), 3);
    assert_eq!(first.eager_refs.len(), 1);
    assert_eq!(first.eager_refs[0].name, "pr-314");
    assert_eq!(first.eager_refs[0].commit_sha, PR_SHA);
    assert!(first.eager_refs.iter().all(|source| {
        source.kind == SourceRefKind::PullRequest && !source.deleted && !source.closed
    }));
    assert!(
        first
            .refs
            .iter()
            .any(|source| { source.kind == SourceRefKind::PullRequest && source.name == "pr-313" })
    );
    assert!(
        first
            .refs
            .iter()
            .any(|source| { source.kind == SourceRefKind::Branch && source.name == "master" })
    );
    assert_eq!(first.checkpoint.etag.as_deref(), Some("pumpkin-pr-etag-v1"));
    assert_eq!(
        first.checkpoint.branch_etag.as_deref(),
        Some("pumpkin-branch-etag-v1")
    );

    let second = client.poll(REPOSITORY, &first.checkpoint).await.unwrap();
    assert!(second.eager_refs.is_empty());
    assert_eq!(second.refs.len(), 3);

    let requests = fixture.requests.lock().unwrap().clone();
    assert!(requests.iter().all(|request| {
        request
            .path_and_query
            .starts_with("/repos/Pumpkin-MC/Pumpkin/")
            && !request.path_and_query.starts_with("/repos/kurbis-host/")
            && request.authorization.as_deref() == Some("Bearer fixture-token")
    }));
    assert!(requests.iter().any(|request| {
        request.path_and_query.contains("/pulls?state=all")
            && request.path_and_query.ends_with("page=2")
    }));
    assert!(requests.iter().any(|request| {
        request.path_and_query.contains("/branches?") && request.path_and_query.ends_with("page=2")
    }));
    for (collection, etag) in [
        ("/pulls?", "pumpkin-pr-etag-v1"),
        ("/branches?", "pumpkin-branch-etag-v1"),
    ] {
        assert!(requests.iter().any(|request| {
            request.path_and_query.contains(collection)
                && request.path_and_query.ends_with("page=1")
                && request.if_none_match.as_deref() == Some(etag)
        }));
        assert!(
            requests.iter().any(|request| {
                request.path_and_query.contains(collection)
                    && request.path_and_query.ends_with("page=1")
                    && request.if_none_match.is_none()
            }),
            "a 304 collection response must be followed by an unconditional page-one fetch"
        );
        assert!(
            requests
                .iter()
                .filter(|request| {
                    request.path_and_query.contains(collection)
                        && request.path_and_query.ends_with("page=2")
                })
                .all(|request| request.if_none_match.is_none())
        );
    }

    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn pumpkin_check_run_get_then_write_uses_public_commit_url_and_exact_target() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (api_base, server) = start_check_run_server(requests.clone()).await;
    let client = GitHubCheckRunClient::new(Client::new(), "fixture-token")
        .unwrap()
        .with_api_base_url(api_base)
        .with_details_base_url("https://panel.example.test/ci/previews")
        .unwrap();
    let commit_hostname = format!("commit-{PR_SHA}.pumpkin.kurbis.host");
    let queued = CheckRunDetails {
        commit_sha: PR_SHA.into(),
        stage: CiBuildStage::Queued,
        repository: REPOSITORY.into(),
        artifact_digest: None,
        failure_stage: None,
        failure_message: None,
        build_log: String::new(),
        preview_hostnames: vec![commit_hostname.clone()],
        expires_at: None,
        details_url: None,
    };

    let id = client.get_or_create(REPOSITORY, &queued).await.unwrap();
    assert_eq!(id, CheckRunId(987654321));
    let completed = CheckRunDetails {
        stage: CiBuildStage::Ready,
        artifact_digest: Some("sha256:fixture-artifact".into()),
        ..queued
    };
    client.update(REPOSITORY, id, &completed).await.unwrap();

    let requests = requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        3,
        "GET idempotency lookup + one POST + one PATCH"
    );
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(
        requests[0].path_and_query,
        format!(
            "/repos/Pumpkin-MC/Pumpkin/commits/{PR_SHA}/check-runs?check_name=Pumpkin%20CI%20build&per_page=100"
        )
    );
    assert_eq!(requests[1].method, Method::POST);
    assert_eq!(
        requests[1].path_and_query,
        "/repos/Pumpkin-MC/Pumpkin/check-runs"
    );
    assert_eq!(requests[1].body["name"], "Pumpkin CI build");
    assert_eq!(requests[1].body["head_sha"], PR_SHA);
    assert_eq!(requests[1].body["status"], "queued");
    assert_eq!(
        requests[1].body["details_url"],
        format!("https://panel.example.test/ci/previews/{commit_hostname}")
    );
    assert_eq!(requests[2].method, Method::PATCH);
    assert_eq!(
        requests[2].path_and_query,
        "/repos/Pumpkin-MC/Pumpkin/check-runs/987654321"
    );
    assert_eq!(requests[2].body["head_sha"], PR_SHA);
    assert_eq!(requests[2].body["status"], "completed");
    assert_eq!(requests[2].body["conclusion"], "success");
    assert!(requests.iter().all(|request| {
        request.authorization.as_deref() == Some("Bearer fixture-token")
            && request
                .path_and_query
                .starts_with("/repos/Pumpkin-MC/Pumpkin/")
    }));

    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
