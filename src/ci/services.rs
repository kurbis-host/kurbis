use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::domain::{BuildKey, CiBuild, CiBuildStage, CiPreview, CiPreviewState};

/// Immutable build identity pinned to the service's checked-in integration patch
/// and native target. API admission uses this only after a concrete commit is
/// requested; it does not authorize eager branch or ancestor builds.
#[derive(Debug, Clone)]
pub struct CiBuildIdentity {
    pub repository: String,
    pub patch_fingerprint: String,
    pub toolchain_version: String,
    pub target_architecture: String,
}

#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub key: BuildKey,
    /// Fork PR builds fetch only this repository's exact PR head commit. The build key remains scoped to the watched base repository.
    pub source_repository: Option<String>,
    pub source_archive: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BuildOutput {
    pub artifact_digest: String,
    pub object_key: String,
    pub size_bytes: i64,
}

#[derive(Debug, Error)]
pub enum CiServiceError {
    #[error("build rejected: {0}")]
    Rejected(String),
    #[error("sandbox requirements are not enforced: {0}")]
    SandboxUnavailable(String),
    #[error("artifact is invalid: {0}")]
    InvalidArtifact(String),
    #[error("CI service failed: {0}")]
    Internal(String),
}

#[derive(Debug, Clone)]
pub struct ControlledBuildOutput {
    pub artifact: Vec<u8>,
    pub build_log: String,
}

#[async_trait]
pub trait BuildCoordinator: Send + Sync {
    async fn ensure_build(&self, request: BuildRequest) -> Result<CiBuild, CiServiceError>;
    async fn transition(
        &self,
        build_id: Uuid,
        stage: CiBuildStage,
        error: Option<&str>,
    ) -> Result<CiBuild, CiServiceError>;
}

#[async_trait]
pub trait ArtifactCache: Send + Sync {
    async fn put(
        &self,
        digest: &str,
        object_key: &str,
        bytes: &[u8],
        expires_at: DateTime<Utc>,
    ) -> Result<BuildOutput, CiServiceError>;
    async fn get(&self, digest: &str) -> Result<Option<Vec<u8>>, CiServiceError>;
    async fn delete_expired(&self, now: DateTime<Utc>) -> Result<u64, CiServiceError>;
}

#[derive(Debug, Clone, Copy)]
pub struct SandboxLimits {
    pub memory_bytes: u64,
    pub cpu_millis: u64,
    pub pid_limit: u32,
    pub disk_bytes: u64,
    pub process_timeout: Duration,
}

#[async_trait]
pub trait UntrustedWorkloadRuntime: Send + Sync {
    async fn verify_sandbox(&self, limits: SandboxLimits) -> Result<(), CiServiceError>;
    async fn start(
        &self,
        preview: &CiPreview,
        artifact: &BuildOutput,
    ) -> Result<(), CiServiceError>;
    async fn stop(&self, preview_id: Uuid, generation: i64) -> Result<(), CiServiceError>;
}

#[async_trait]
pub trait CiGatewayAdmission: Send + Sync {
    async fn ensure_preview(&self, hostname: &str) -> Result<CiPreviewState, CiServiceError>;
    async fn status(&self, hostname: &str) -> Result<Option<CiPreview>, CiServiceError>;
}

#[derive(Clone)]
pub struct HttpCiGatewayAdmission {
    client: Client,
    control_plane_url: String,
    internal_token: String,
}
impl HttpCiGatewayAdmission {
    pub fn new(
        client: Client,
        control_plane_url: impl Into<String>,
        internal_token: impl Into<String>,
    ) -> Result<Self, CiServiceError> {
        let internal_token = internal_token.into();
        if internal_token.trim().is_empty() {
            return Err(CiServiceError::Internal(
                "internal token cannot be empty".into(),
            ));
        }
        Ok(Self {
            client,
            control_plane_url: control_plane_url.into().trim_end_matches('/').into(),
            internal_token,
        })
    }
}
#[derive(Debug, Serialize)]
struct AdmissionRequest<'a> {
    hostname: &'a str,
}
#[derive(Debug, Deserialize)]
struct AdmissionResponse {
    state: CiPreviewState,
}
#[async_trait]
impl CiGatewayAdmission for HttpCiGatewayAdmission {
    async fn ensure_preview(&self, hostname: &str) -> Result<CiPreviewState, CiServiceError> {
        let response = self
            .client
            .post(format!(
                "{}/internal/ci/previews/admit",
                self.control_plane_url
            ))
            .bearer_auth(&self.internal_token)
            .json(&AdmissionRequest { hostname })
            .send()
            .await
            .map_err(|e| CiServiceError::Internal(e.to_string()))?;
        if !response.status().is_success() && response.status().as_u16() != 202 {
            return Err(CiServiceError::Internal(format!(
                "CI admission rejected with {}",
                response.status()
            )));
        }
        Ok(response
            .json::<AdmissionResponse>()
            .await
            .map_err(|e| CiServiceError::Internal(e.to_string()))?
            .state)
    }
    async fn status(&self, hostname: &str) -> Result<Option<CiPreview>, CiServiceError> {
        let response = self
            .client
            .get(format!(
                "{}/internal/ci/previews/status/{}",
                self.control_plane_url,
                percent_encode_path(hostname)
            ))
            .bearer_auth(&self.internal_token)
            .send()
            .await
            .map_err(|e| CiServiceError::Internal(e.to_string()))?;
        if !response.status().is_success() {
            return Err(CiServiceError::Internal(format!(
                "CI status rejected with {}",
                response.status()
            )));
        }
        #[derive(Deserialize)]
        struct Status {
            preview: Option<CiPreview>,
        }
        Ok(response
            .json::<Status>()
            .await
            .map_err(|e| CiServiceError::Internal(e.to_string()))?
            .preview)
    }
}

fn percent_encode_path(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

pub fn validate_workload_limits(limits: SandboxLimits) -> Result<(), CiServiceError> {
    if limits.memory_bytes == 0
        || limits.cpu_millis == 0
        || limits.pid_limit == 0
        || limits.disk_bytes == 0
        || limits.process_timeout.is_zero()
    {
        return Err(CiServiceError::SandboxUnavailable(
            "all workload limits must be non-zero".into(),
        ));
    }
    // This helper validates the requested contract only. Production callers must
    // then run their concrete rootless Docker capability probe; environment
    // variables are never accepted as sandbox evidence.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sandbox_admission_fails_closed_for_missing_limits() {
        let limits = SandboxLimits {
            memory_bytes: 0,
            cpu_millis: 1,
            pid_limit: 1,
            disk_bytes: 1,
            process_timeout: Duration::from_secs(1),
        };
        assert!(matches!(
            validate_workload_limits(limits),
            Err(CiServiceError::SandboxUnavailable(_))
        ));
    }
}

#[derive(Clone)]
pub struct ObjectArtifactCache {
    pool: sqlx::PgPool,
    store: std::sync::Arc<dyn object_store::ObjectStore>,
}
impl ObjectArtifactCache {
    pub fn new(pool: sqlx::PgPool, store: std::sync::Arc<dyn object_store::ObjectStore>) -> Self {
        Self { pool, store }
    }
}
#[async_trait]
impl ArtifactCache for ObjectArtifactCache {
    async fn put(
        &self,
        digest: &str,
        object_key: &str,
        bytes: &[u8],
        expires_at: DateTime<Utc>,
    ) -> Result<BuildOutput, CiServiceError> {
        let actual_digest = blake3::hash(bytes).to_hex().to_string();
        if digest != actual_digest
            || object_key.is_empty()
            || object_key.len() > 1024
            || !object_key.starts_with("ci/")
        {
            return Err(CiServiceError::InvalidArtifact(
                "artifact digest or CI object key is invalid".into(),
            ));
        }
        let path = object_store::path::Path::parse(object_key)
            .map_err(|e| CiServiceError::InvalidArtifact(e.to_string()))?;
        self.store
            .put(&path, bytes::Bytes::copy_from_slice(bytes).into())
            .await
            .map_err(|e| CiServiceError::Internal(e.to_string()))?;
        let size_bytes = i64::try_from(bytes.len()).map_err(|_| {
            CiServiceError::InvalidArtifact("artifact exceeds signed size range".into())
        })?;
        sqlx::query("INSERT INTO ci_artifacts(digest, object_key, size_bytes, expires_at) VALUES($1,$2,$3,$4) ON CONFLICT(digest) DO UPDATE SET object_key=EXCLUDED.object_key, size_bytes=EXCLUDED.size_bytes, expires_at=GREATEST(ci_artifacts.expires_at, EXCLUDED.expires_at)")
            .bind(digest).bind(object_key).bind(size_bytes).bind(expires_at).execute(&self.pool).await.map_err(|e| CiServiceError::Internal(e.to_string()))?;
        Ok(BuildOutput {
            artifact_digest: actual_digest,
            object_key: object_key.to_owned(),
            size_bytes,
        })
    }
    async fn get(&self, digest: &str) -> Result<Option<Vec<u8>>, CiServiceError> {
        let object_key: Option<String> = sqlx::query_scalar(
            "SELECT object_key FROM ci_artifacts WHERE digest=$1 AND expires_at > now()",
        )
        .bind(digest)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CiServiceError::Internal(e.to_string()))?;
        let Some(object_key) = object_key else {
            return Ok(None);
        };
        let path = object_store::path::Path::parse(object_key)
            .map_err(|e| CiServiceError::InvalidArtifact(e.to_string()))?;
        let result = match self.store.get(&path).await {
            Ok(result) => result,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(CiServiceError::Internal(error.to_string())),
        };
        let bytes = result
            .bytes()
            .await
            .map_err(|e| CiServiceError::Internal(e.to_string()))?;
        if blake3::hash(&bytes).to_hex().as_str() != digest {
            return Err(CiServiceError::InvalidArtifact(
                "cached artifact digest mismatch".into(),
            ));
        }
        Ok(Some(bytes.to_vec()))
    }
    async fn delete_expired(&self, now: DateTime<Utc>) -> Result<u64, CiServiceError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CiServiceError::Internal(e.to_string()))?;
        let expired: Vec<(String, String)> = sqlx::query_as(
            "SELECT artifact.digest, artifact.object_key FROM ci_artifacts artifact\n             WHERE artifact.expires_at <= $1 AND NOT EXISTS (SELECT 1 FROM ci_builds build WHERE build.artifact_digest = artifact.digest AND build.expires_at > $1)\n             ORDER BY artifact.expires_at LIMIT 128 FOR UPDATE OF artifact SKIP LOCKED",
        ).bind(now).fetch_all(&mut *tx).await.map_err(|e| CiServiceError::Internal(e.to_string()))?;
        for (digest, object_key) in &expired {
            let path = object_store::path::Path::parse(object_key)
                .map_err(|e| CiServiceError::InvalidArtifact(e.to_string()))?;
            match self.store.delete(&path).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(error) => return Err(CiServiceError::Internal(error.to_string())),
            }
            sqlx::query("DELETE FROM ci_artifacts WHERE digest = $1")
                .bind(digest)
                .execute(&mut *tx)
                .await
                .map_err(|e| CiServiceError::Internal(e.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|e| CiServiceError::Internal(e.to_string()))?;
        Ok(expired.len() as u64)
    }
}
