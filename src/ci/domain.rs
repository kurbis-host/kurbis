use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CI_ROOT_DOMAIN: &str = "pumpkin.kurbis.host";
pub const BUILD_TTL_HOURS: i64 = 72;
pub const POLL_INTERVAL_SECONDS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CiRefKind {
    PullRequest,
    Branch,
    Commit,
}

impl CiRefKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PullRequest => "pull_request",
            Self::Branch => "branch",
            Self::Commit => "commit",
        }
    }
}

impl fmt::Display for CiRefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CiRefKind {
    type Err = InvalidCiValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pull_request" => Ok(Self::PullRequest),
            "branch" => Ok(Self::Branch),
            "commit" => Ok(Self::Commit),
            _ => Err(InvalidCiValue::new("unknown CI ref kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CiBuildStage {
    Queued,
    Fetching,
    Patching,
    Compiling,
    Packaging,
    Ready,
    Failed,
    Expired,
}

impl CiBuildStage {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Ready | Self::Failed | Self::Expired)
    }

    pub const fn github_conclusion(self) -> Option<&'static str> {
        match self {
            Self::Ready => Some("success"),
            Self::Failed => Some("failure"),
            Self::Expired => Some("cancelled"),
            _ => None,
        }
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Queued, Self::Fetching | Self::Failed | Self::Expired)
                | (
                    Self::Fetching,
                    Self::Patching | Self::Failed | Self::Expired
                )
                | (
                    Self::Patching,
                    Self::Compiling | Self::Failed | Self::Expired
                )
                | (
                    Self::Compiling,
                    Self::Packaging | Self::Failed | Self::Expired
                )
                | (Self::Packaging, Self::Ready | Self::Failed | Self::Expired)
                | (Self::Ready, Self::Expired)
        )
    }
}

impl fmt::Display for CiBuildStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Queued => "queued",
            Self::Fetching => "fetching",
            Self::Patching => "patching",
            Self::Compiling => "compiling",
            Self::Packaging => "packaging",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Expired => "expired",
        })
    }
}

impl FromStr for CiBuildStage {
    type Err = InvalidCiValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "fetching" => Ok(Self::Fetching),
            "patching" => Ok(Self::Patching),
            "compiling" => Ok(Self::Compiling),
            "packaging" => Ok(Self::Packaging),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            "expired" => Ok(Self::Expired),
            _ => Err(InvalidCiValue::new("unknown CI build stage")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CiPreviewState {
    Lobby,
    Queued,
    Fetching,
    Patching,
    Compiling,
    Packaging,
    Booting,
    Ready,
    Running,
    Stopping,
    Failed,
    Expired,
}

impl CiPreviewState {
    pub const fn from_build(stage: CiBuildStage) -> Self {
        match stage {
            CiBuildStage::Queued => Self::Queued,
            CiBuildStage::Fetching => Self::Fetching,
            CiBuildStage::Patching => Self::Patching,
            CiBuildStage::Compiling => Self::Compiling,
            CiBuildStage::Packaging => Self::Packaging,
            CiBuildStage::Ready => Self::Ready,
            CiBuildStage::Failed => Self::Failed,
            CiBuildStage::Expired => Self::Expired,
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Expired)
    }
}

impl FromStr for CiPreviewState {
    type Err = InvalidCiValue;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "lobby" => Ok(Self::Lobby),
            "queued" => Ok(Self::Queued),
            "fetching" => Ok(Self::Fetching),
            "patching" => Ok(Self::Patching),
            "compiling" => Ok(Self::Compiling),
            "packaging" => Ok(Self::Packaging),
            "booting" => Ok(Self::Booting),
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "stopping" => Ok(Self::Stopping),
            "failed" => Ok(Self::Failed),
            "expired" => Ok(Self::Expired),
            _ => Err(InvalidCiValue::new("unknown CI preview state")),
        }
    }
}

impl fmt::Display for CiPreviewState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lobby => "lobby",
            Self::Queued => "queued",
            Self::Fetching => "fetching",
            Self::Patching => "patching",
            Self::Compiling => "compiling",
            Self::Packaging => "packaging",
            Self::Booting => "booting",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
            Self::Expired => "expired",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BuildKey {
    pub repository: String,
    pub commit_sha: String,
    pub patch_fingerprint: String,
    pub toolchain_version: String,
    pub target_architecture: String,
}

impl BuildKey {
    pub fn new(
        repository: impl Into<String>,
        commit_sha: impl Into<String>,
        patch_fingerprint: impl Into<String>,
        toolchain_version: impl Into<String>,
        target_architecture: impl Into<String>,
    ) -> Result<Self, InvalidCiValue> {
        let commit_sha = canonical_commit_sha(&commit_sha.into())?;
        let key = Self {
            repository: repository.into(),
            commit_sha,
            patch_fingerprint: patch_fingerprint.into(),
            toolchain_version: toolchain_version.into(),
            target_architecture: target_architecture.into(),
        };
        key.validate()?;
        Ok(key)
    }

    pub fn validate(&self) -> Result<(), InvalidCiValue> {
        if self.repository.trim().is_empty()
            || self.patch_fingerprint.trim().is_empty()
            || self.toolchain_version.trim().is_empty()
            || self.target_architecture.trim().is_empty()
        {
            return Err(InvalidCiValue::new("build key fields cannot be empty"));
        }
        validate_commit_sha(&self.commit_sha)
    }

    pub fn cache_key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for field in [
            self.repository.as_str(),
            self.commit_sha.as_str(),
            self.patch_fingerprint.as_str(),
            self.toolchain_version.as_str(),
            self.target_architecture.as_str(),
        ] {
            hasher.update(&(field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CiHostname {
    pub value: String,
    pub kind: CiRefKind,
}

impl CiHostname {
    pub fn for_ref(
        kind: CiRefKind,
        value: &str,
        root_domain: &str,
    ) -> Result<Self, InvalidCiValue> {
        let label = match kind {
            CiRefKind::PullRequest => format!("pr-{}", parse_pr_number(value)?),
            CiRefKind::Branch => format!("branch-{}", slugify_branch(value)?),
            CiRefKind::Commit => format!("commit-{}", canonical_commit_sha(value)?),
        };
        let root = normalize_root_domain(root_domain)?;
        let hostname = format!("{label}.{root}");
        if hostname.len() > 253 || label.len() > 63 {
            return Err(InvalidCiValue::new("CI hostname exceeds DNS limits"));
        }
        Ok(Self {
            value: hostname,
            kind,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CiBuild {
    pub id: Uuid,
    pub key: BuildKey,
    pub stage: CiBuildStage,
    pub artifact_digest: Option<String>,
    pub failure_stage: Option<CiBuildStage>,
    pub failure_message: Option<String>,
    pub build_log: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CiPublicPreviewStatus {
    pub hostname: String,
    pub commit_sha: String,
    pub kind: CiRefKind,
    pub state: CiPreviewState,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CiPreview {
    pub id: Uuid,
    pub repository: String,
    pub commit_sha: String,
    pub alias: CiHostname,
    pub state: CiPreviewState,
    pub build_id: Uuid,
    pub runtime_generation: i64,
    pub failure_message: Option<String>,
    pub build_log: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidCiValue {
    pub message: &'static str,
}

impl InvalidCiValue {
    pub const fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl fmt::Display for InvalidCiValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for InvalidCiValue {}

pub fn canonical_commit_sha(value: &str) -> Result<String, InvalidCiValue> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(InvalidCiValue::new(
            "commit SHA must be a full 40-character hexadecimal SHA",
        ));
    }
    Ok(value.to_ascii_lowercase())
}

pub fn validate_commit_sha(value: &str) -> Result<(), InvalidCiValue> {
    let canonical = canonical_commit_sha(value)?;
    if canonical != value {
        return Err(InvalidCiValue::new(
            "commit SHA must use canonical lowercase hexadecimal",
        ));
    }
    Ok(())
}

pub fn parse_pr_number(value: &str) -> Result<u64, InvalidCiValue> {
    let number = value.strip_prefix("pr-").unwrap_or(value);
    let parsed = number
        .parse::<u64>()
        .map_err(|_| InvalidCiValue::new("pull request number must be a positive integer"))?;
    if parsed == 0 {
        return Err(InvalidCiValue::new("pull request number must be positive"));
    }
    Ok(parsed)
}

pub fn slugify_branch(value: &str) -> Result<String, InvalidCiValue> {
    let value = value.trim();
    if value.is_empty() {
        return Err(InvalidCiValue::new("branch name cannot be empty"));
    }
    let mut slug = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() {
            slug.push(byte.to_ascii_lowercase() as char);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(InvalidCiValue::new(
            "branch name contains no DNS-safe characters",
        ));
    }
    // Slugification can collide even for short refs (feature/foo and
    // feature-foo). A suffix on every alias makes the DNS mapping injective
    // except for the explicit hash collision probability.
    let hash = blake3::hash(value.as_bytes()).to_hex();
    slug.truncate(38);
    while slug.ends_with('-') {
        slug.pop();
    }
    slug.push('-');
    slug.push_str(&hash[..12]);
    debug_assert!(slug.len() <= 51);
    Ok(slug)
}

pub fn normalize_root_domain(value: &str) -> Result<String, InvalidCiValue> {
    let root = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if root.is_empty() || root.len() > 253 || root.split('.').any(|part| part.is_empty()) {
        return Err(InvalidCiValue::new("root domain is invalid"));
    }
    if root.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return Err(InvalidCiValue::new(
            "root domain contains invalid DNS labels",
        ));
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_aliases_are_safe_and_collision_resistant() {
        let short = slugify_branch("feature/Hello_World").unwrap();
        assert!(short.starts_with("feature-hello-world-"));
        assert_ne!(
            slugify_branch("feature/foo").unwrap(),
            slugify_branch("feature-foo").unwrap()
        );
        let long = slugify_branch(&"a/".repeat(80)).unwrap();
        assert!(long.len() <= 63);
        assert!(long.rsplit_once('-').unwrap().1.len() == 12);
        assert_ne!(long, slugify_branch(&"b/".repeat(80)).unwrap());
    }

    #[test]
    fn aliases_use_full_commit_shas_and_dns_safe_labels() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let hostname = CiHostname::for_ref(CiRefKind::Commit, sha, "pumpkin.kurbis.host").unwrap();
        assert_eq!(hostname.value, format!("commit-{sha}.pumpkin.kurbis.host"));
        assert!(CiHostname::for_ref(CiRefKind::Commit, "abc", "pumpkin.kurbis.host").is_err());
    }

    #[test]
    fn build_stage_transitions_fail_closed() {
        assert!(CiBuildStage::Queued.can_transition_to(CiBuildStage::Fetching));
        assert!(CiBuildStage::Packaging.can_transition_to(CiBuildStage::Ready));
        assert!(!CiBuildStage::Ready.can_transition_to(CiBuildStage::Compiling));
        assert!(!CiBuildStage::Failed.can_transition_to(CiBuildStage::Ready));
    }
}
