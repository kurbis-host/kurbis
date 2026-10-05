#![allow(unused_imports)]
#[allow(dead_code)]
#[path = "common/mod.rs"]
mod common;

use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::Utc;
use kurbis_hoster::ci::{
    BuildKey, CiBuildStage, CiHostname, CiPreviewRepository, CiRefKind, PollCheckpoint, PollResult,
    SourceControlClient, SourceRef, SourceRefKind,
};
use kurbis_hoster::{db, routing::RouteRepository};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

static CI_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
struct FixtureSource {
    commit_sha: std::sync::Arc<std::sync::RwLock<String>>,
    deleted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    include_branch: std::sync::Arc<std::sync::atomic::AtomicBool>,
    complete_branches: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Default for FixtureSource {
    fn default() -> Self {
        Self {
            commit_sha: std::sync::Arc::new(std::sync::RwLock::new(
                "0123456789abcdef0123456789abcdef01234567".into(),
            )),
            deleted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            include_branch: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            complete_branches: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}
#[async_trait]
impl SourceControlClient for FixtureSource {
    async fn poll(
        &self,
        _repository: &str,
        _checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Ok(PollResult {
            refs: if self
                .include_branch
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                vec![SourceRef {
                    kind: SourceRefKind::Branch,
                    name: "main".into(),
                    number: None,
                    commit_sha: self.commit_sha.read().unwrap().clone(),
                    source_repository: Some("kurbis-host/Pumpkin".into()),
                    closed: false,
                    deleted: self.deleted.load(std::sync::atomic::Ordering::SeqCst),
                    updated_at: Some(Utc::now()),
                }]
            } else {
                Vec::new()
            },
            eager_refs: Vec::new(),
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..Default::default()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: if self
                .complete_branches
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                vec![SourceRefKind::Branch]
            } else {
                Vec::new()
            },
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

fn unique_ci_root_domain() -> String {
    format!("pumpkin-{}.kurbis.host", Uuid::new_v4().simple())
}

async fn pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = db::connect(&url).await.unwrap();
    db::migrate(&pool).await.unwrap();
    pool
}

#[derive(Clone)]
struct PullRequestFixtureSource {
    fetches: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    head_sha: std::sync::Arc<std::sync::RwLock<String>>,
}
#[async_trait]
impl SourceControlClient for PullRequestFixtureSource {
    async fn poll(
        &self,
        _repository: &str,
        _checkpoint: &PollCheckpoint,
    ) -> Result<PollResult, kurbis_hoster::ci::PollError> {
        Ok(PollResult {
            refs: vec![SourceRef {
                kind: SourceRefKind::PullRequest,
                name: "pr-314".into(),
                number: Some(314),
                commit_sha: self.head_sha.read().unwrap().clone(),
                source_repository: Some("contributor/Pumpkin".into()),
                closed: false,
                deleted: false,
                updated_at: None,
            }],
            eager_refs: vec![SourceRef {
                kind: SourceRefKind::PullRequest,
                name: "pr-314".into(),
                number: Some(314),
                commit_sha: self.head_sha.read().unwrap().clone(),
                source_repository: Some("contributor/Pumpkin".into()),
                closed: false,
                deleted: false,
                updated_at: None,
            }],
            checkpoint: PollCheckpoint {
                last_successful_poll: Some(Utc::now()),
                ..Default::default()
            },
            not_modified: false,
            retry_after: None,
            complete_kinds: vec![SourceRefKind::PullRequest],
        })
    }

    async fn fetch_commit(
        &self,
        repository: &str,
        commit_sha: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::PollError> {
        self.fetches
            .lock()
            .unwrap()
            .push((repository.to_owned(), commit_sha.to_owned()));
        Ok(format!("exact-fork-commit:{commit_sha}").into_bytes())
    }
}

#[path = "ci_preview/basics.rs"]
mod basics;
#[path = "ci_preview/builders.rs"]
mod builders;
#[path = "ci_preview/polling.rs"]
mod polling;

#[derive(Default)]
struct TestBuildComponents {
    compile_calls: std::sync::atomic::AtomicUsize,
    transient_compile_failures: std::sync::atomic::AtomicUsize,
    permanent_compile_failures: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl kurbis_hoster::ci::PatchApplier for TestBuildComponents {
    async fn apply(
        &self,
        source_archive: Vec<u8>,
        fingerprint: &str,
    ) -> Result<Vec<u8>, kurbis_hoster::ci::BuildPipelineError> {
        let mut patched = source_archive;
        patched.extend_from_slice(fingerprint.as_bytes());
        Ok(patched)
    }
}
#[async_trait]
impl kurbis_hoster::ci::ControlledBuilder for TestBuildComponents {
    async fn verify_sandbox(
        &self,
        limits: kurbis_hoster::ci::SandboxLimits,
    ) -> Result<(), kurbis_hoster::ci::BuildPipelineError> {
        if limits.memory_bytes == 0
            || limits.cpu_millis == 0
            || limits.pid_limit == 0
            || limits.disk_bytes == 0
        {
            return Err(kurbis_hoster::ci::BuildPipelineError::Sandbox(
                "missing resource limit".into(),
            ));
        }
        Ok(())
    }
    async fn compile_and_package(
        &self,
        patched_source: Vec<u8>,
        _key: &BuildKey,
    ) -> Result<kurbis_hoster::ci::ControlledBuildOutput, kurbis_hoster::ci::BuildPipelineError>
    {
        self.compile_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .transient_compile_failures
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(kurbis_hoster::ci::BuildPipelineError::Transient {
                stage: CiBuildStage::Compiling,
                message: "temporary compiler container interruption".into(),
            });
        }
        if self
            .permanent_compile_failures
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            return Err(kurbis_hoster::ci::BuildPipelineError::Permanent {
                stage: CiBuildStage::Compiling,
                message: "deterministic compiler diagnostic".into(),
            });
        }
        let mut artifact = patched_source;
        artifact.extend_from_slice(b"verified-pumpkin-artifact");
        Ok(kurbis_hoster::ci::ControlledBuildOutput {
            artifact,
            build_log: "cargo build succeeded\n".into(),
        })
    }
}
struct TestArtifactCache;
#[async_trait]
impl kurbis_hoster::ci::ArtifactCache for TestArtifactCache {
    async fn put(
        &self,
        digest: &str,
        object_key: &str,
        bytes: &[u8],
        _expires_at: chrono::DateTime<Utc>,
    ) -> Result<kurbis_hoster::ci::BuildOutput, kurbis_hoster::ci::CiServiceError> {
        Ok(kurbis_hoster::ci::BuildOutput {
            artifact_digest: digest.to_owned(),
            object_key: object_key.to_owned(),
            size_bytes: bytes.len() as i64,
        })
    }
    async fn get(
        &self,
        _digest: &str,
    ) -> Result<Option<Vec<u8>>, kurbis_hoster::ci::CiServiceError> {
        Ok(None)
    }
    async fn delete_expired(
        &self,
        _now: chrono::DateTime<Utc>,
    ) -> Result<u64, kurbis_hoster::ci::CiServiceError> {
        Ok(0)
    }
}

struct TestCheckRuns {
    id: i64,
    stages: std::sync::Mutex<Vec<CiBuildStage>>,
}
#[async_trait]
impl kurbis_hoster::ci::CheckRunClient for TestCheckRuns {
    async fn create(
        &self,
        _repository: &str,
        _details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<kurbis_hoster::ci::CheckRunId, kurbis_hoster::ci::CheckRunError> {
        Ok(kurbis_hoster::ci::CheckRunId(self.id))
    }
    async fn update(
        &self,
        _repository: &str,
        id: kurbis_hoster::ci::CheckRunId,
        details: &kurbis_hoster::ci::CheckRunDetails,
    ) -> Result<(), kurbis_hoster::ci::CheckRunError> {
        assert_eq!(id.0, self.id);
        assert_eq!(details.commit_sha.len(), 40);
        self.stages.lock().unwrap().push(details.stage);
        Ok(())
    }
}

#[path = "ci_preview/bootstrap_recovery.rs"]
mod bootstrap_recovery;
#[path = "ci_preview/runtime.rs"]
mod runtime;
#[path = "ci_preview/streaming.rs"]
mod streaming;
