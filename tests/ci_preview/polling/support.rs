use super::*;

pub(crate) struct IdempotentCheckRuns {
    pub(crate) id: i64,
    pub(crate) create_count: std::sync::atomic::AtomicUsize,
    pub(crate) update_stages: std::sync::Mutex<Vec<CiBuildStage>>,
}
#[async_trait]
impl kurbis_hoster::ci::CheckRunClient for IdempotentCheckRuns {
    async fn create(
        &self,
        _repository: &str,
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        if let Some(url) = details.details_url.as_deref() {
            assert!(url.starts_with("https://panel.example.test/ci/previews/commit-"));
        }
        let sequence = self
            .create_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst) as i64;
        Ok(kurbis_hoster::ci::CheckRunId(self.id + sequence))
    }

    async fn get_or_create(
        &self,
        repository: &str,
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        self.create(repository, details).await
    }

    async fn update(
        &self,
        _repository: &str,
        id: kurbis_hoster::ci::CheckRunId,
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<(), kurbis_hoster::ci::CheckRunError> {
        assert!(id.0 >= self.id);
        if let Some(url) = details.details_url.as_deref() {
            assert!(url.starts_with("https://panel.example.test/ci/previews/commit-"));
        }
        assert!(details.build_log.len() <= 8 * 1024);
        assert!(
            details
                .preview_hostnames
                .iter()
                .any(|host| host.starts_with("commit-"))
        );
        self.update_stages.lock().unwrap().push(details.stage);
        Ok(())
    }
}

pub(crate) struct PermissionDeniedCheckRuns {
    pub(crate) calls: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl kurbis_hoster::ci::CheckRunClient for PermissionDeniedCheckRuns {
    async fn create(
        &self,
        _repository: &str,
        _details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(kurbis_hoster::ci::CheckRunError::Http {
            status: StatusCode::FORBIDDEN,
            message: "fixture permission denied".into(),
        })
    }

    async fn update(
        &self,
        _repository: &str,
        _id: kurbis_hoster::ci::CheckRunId,
        _details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<(), kurbis_hoster::ci::CheckRunError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(kurbis_hoster::ci::CheckRunError::Http {
            status: StatusCode::FORBIDDEN,
            message: "fixture permission denied".into(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct EmptyPollSource;
#[async_trait]
impl SourceControlClient for EmptyPollSource {
    async fn poll(
        &self,
        _repository: &str,
        checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Ok(PollResult {
            refs: Vec::new(),
            eager_refs: Vec::new(),
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..checkpoint.clone()
            },
            not_modified: true,
            retry_after: None,
            complete_kinds: Vec::new(),
        })
    }

    async fn fetch_commit(
        &self,
        _repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        Ok(format!("recovered-pr-head:{commit_sha}").into_bytes())
    }
}

#[derive(Clone)]
pub(crate) struct MutableBranchSource {
    pub(crate) name: String,
    pub(crate) commit: std::sync::Arc<std::sync::RwLock<String>>,
    pub(crate) fetches: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}
#[async_trait]
impl SourceControlClient for MutableBranchSource {
    async fn poll(
        &self,
        repository: &str,
        checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        let commit_sha = self.commit.read().unwrap().clone();
        Ok(PollResult {
            refs: vec![SourceRef {
                kind: SourceRefKind::Branch,
                name: self.name.clone(),
                number: None,
                commit_sha,
                source_repository: Some(repository.to_owned()),
                closed: false,
                deleted: false,
                updated_at: Some(Utc::now()),
            }],
            eager_refs: Vec::new(),
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..checkpoint.clone()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: vec![SourceRefKind::Branch],
        })
    }

    async fn fetch_commit(
        &self,
        _repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        self.fetches.lock().unwrap().push(commit_sha.to_owned());
        Ok(format!("exact-branch-source:{commit_sha}").into_bytes())
    }
}
pub(crate) fn signed_github_request(
    delivery_id: Uuid,
    event_name: &str,
    body: Vec<u8>,
    secret: &str,
    corrupt_signature: bool,
) -> Request<Body> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(&body);
    let signature = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let signature = if corrupt_signature {
        let mut corrupt = signature.into_bytes();
        corrupt[0] = if corrupt[0] == b'0' { b'1' } else { b'0' };
        format!("sha256={}", String::from_utf8(corrupt).unwrap())
    } else {
        format!("sha256={signature}")
    };
    Request::builder()
        .method("POST")
        .uri("/webhooks/github")
        .header("x-github-delivery", delivery_id.to_string())
        .header("x-github-event", event_name)
        .header("x-hub-signature-256", signature)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}
#[derive(Clone, Default)]
pub(crate) struct Pr3865GithubFixture {
    pub(crate) requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

pub(crate) async fn pr3865_github_api(
    axum::extract::State(fixture): axum::extract::State<Pr3865GithubFixture>,
    request: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    fixture
        .requests
        .lock()
        .unwrap()
        .push(format!("{path}?{query}"));

    if path.ends_with("/pulls") && query.contains("page=1") {
        if request
            .headers()
            .get(axum::http::header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            == Some("pr3865-v1")
        {
            return axum::response::Response::builder()
                .status(axum::http::StatusCode::NOT_MODIFIED)
                .body(axum::body::Body::empty())
                .unwrap();
        }
        return axum::response::Response::builder()
            .status(axum::http::StatusCode::OK)
            .header(axum::http::header::ETAG, "pr3865-v1")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(
                serde_json::json!([{
                    "number": 3865,
                    "state": "open",
                    "updated_at": "2026-10-08T00:00:00Z",
                    "head": {
                        "sha": "c80f6fd0123456789abcdef0123456789abcdef0",
                        "repository": {"full_name": "contributor/Pumpkin"}
                    }
                }])
                .to_string(),
            ))
            .unwrap();
    }
    if path.ends_with("/pulls") || path.ends_with("/branches") {
        return axum::response::Response::builder()
            .status(axum::http::StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from("[]"))
            .unwrap();
    }
    if path.contains("/tarball/") {
        return axum::response::Response::builder()
            .status(axum::http::StatusCode::OK)
            .body(axum::body::Body::from("fixture-pr-3865-source"))
            .unwrap();
    }
    axum::response::Response::builder()
        .status(axum::http::StatusCode::NOT_FOUND)
        .body(axum::body::Body::empty())
        .unwrap()
}

pub(crate) struct PausedPollSource {
    pub(crate) started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub(crate) resume: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    pub(crate) calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl SourceControlClient for PausedPollSource {
    async fn poll(
        &self,
        _repository: &str,
        _checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            if let Some(started) = self.started.lock().unwrap().take() {
                let _ = started.send(());
            }
            if let Some(resume) = self.resume.lock().await.take() {
                let _ = resume.await;
            }
        }
        Ok(PollResult {
            refs: Vec::new(),
            eager_refs: Vec::new(),
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..Default::default()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: Vec::new(),
        })
    }

    async fn fetch_commit(
        &self,
        _repository: &str,
        _commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        Ok(Vec::new())
    }
}

pub(crate) async fn wait_for_poll_lease_release(pool: &PgPool, repository_id: Uuid) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let released: bool = sqlx::query_scalar(
                "SELECT poll_lease_token IS NULL FROM ci_watched_repositories WHERE id = $1",
            )
            .bind(repository_id)
            .fetch_one(pool)
            .await
            .unwrap();
            if released {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled polling releases its lease promptly");
}

pub(crate) struct FailingPollSource;

#[async_trait]
impl SourceControlClient for FailingPollSource {
    async fn poll(
        &self,
        _repository: &str,
        _checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Err(kurbis_hoster::ci::PollError::InvalidResponse(
            "fixture provider failure".into(),
        ))
    }

    async fn fetch_commit(
        &self,
        _repository: &str,
        _commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        Ok(Vec::new())
    }
}

pub(crate) struct CheckpointPollSource(pub(crate) &'static str);

#[async_trait]
impl SourceControlClient for CheckpointPollSource {
    async fn poll(
        &self,
        _repository: &str,
        _checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Ok(PollResult {
            refs: Vec::new(),
            eager_refs: Vec::new(),
            checkpoint: PollCheckpoint {
                etag: Some(self.0.into()),
                last_successful_poll: Some(Utc::now()),
                ..Default::default()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: Vec::new(),
        })
    }

    async fn fetch_commit(
        &self,
        _repository: &str,
        _commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        Ok(Vec::new())
    }
}
pub(crate) struct HistoricalRefsSource {
    pub(crate) refs: Vec<SourceRef>,
}

#[async_trait]
impl SourceControlClient for HistoricalRefsSource {
    async fn poll(
        &self,
        _repository: &str,
        _checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Ok(PollResult {
            refs: self.refs.clone(),
            eager_refs: self
                .refs
                .iter()
                .filter(|source_ref| {
                    source_ref.kind == SourceRefKind::PullRequest && !source_ref.closed
                })
                .cloned()
                .collect(),
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..Default::default()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: vec![SourceRefKind::PullRequest, SourceRefKind::Branch],
        })
    }

    async fn fetch_commit(
        &self,
        _repository: &str,
        _commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        Ok(Vec::new())
    }
}
