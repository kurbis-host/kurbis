use std::{
    env, fmt,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::Deserialize;

mod ai;

pub use ai::{AiConfig, AiModelEntry, AiProvider, parse_usd_micros};

#[derive(Clone)]
pub struct CommonConfig {
    pub database_url: String,
}

impl fmt::Debug for CommonConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommonConfig")
            .field("database_url", &"[REDACTED]")
            .finish()
    }
}

impl CommonConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: required("DATABASE_URL")?,
        })
    }
}

#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone)]
pub struct ControlPlaneConfig {
    pub common: CommonConfig,
    pub listen: SocketAddr,
    pub public_base_url: String,
    pub root_domain: String,
    pub internal_token: String,
    /// Signed webhook delivery is deliberately dormant unless explicitly enabled.
    pub ci_github_webhook_enabled: bool,
    pub ci_github_webhook_secret: Option<SecretString>,
    pub oauth: OAuthConfig,
}

impl fmt::Debug for ControlPlaneConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlPlaneConfig")
            .field("common", &self.common)
            .field("listen", &self.listen)
            .field("public_base_url", &self.public_base_url)
            .field("root_domain", &self.root_domain)
            .field("internal_token", &"[REDACTED]")
            .field("ci_github_webhook_enabled", &self.ci_github_webhook_enabled)
            .field("ci_github_webhook_secret", &self.ci_github_webhook_secret)
            .field("oauth", &self.oauth)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CiBuilderBackend {
    Docker,
    Bubblewrap {
        executable: PathBuf,
        rootfs: PathBuf,
        toolchain: PathBuf,
        workspace_root: PathBuf,
        source_preparer: PathBuf,
    },
    BubblewrapSsh {
        target: String,
        runner: PathBuf,
        executable: String,
        rootfs: String,
        toolchain: String,
        workspace_root: String,
        source_preparer: String,
    },
}

#[derive(Clone)]
pub struct CiConfig {
    pub github_pat: String,
    pub repository: String,
    pub github_api_base_url: String,
    pub default_branch: String,
    pub patch_fingerprint: String,
    pub toolchain_version: String,
    pub target_architecture: String,
    pub builder_image: String,
    pub builder_docker_host: String,
    pub builder_backend: CiBuilderBackend,
    /// Maximum number of concurrent build slots per concrete builder identity.
    pub build_concurrency: u16,
    pub github_min_request_interval: Duration,
    pub github_rate_limit_max_backoff: Duration,
    pub github_rate_limit_jitter: f64,
    pub plugin_source: PathBuf,
    /// Token for posting GitHub Check Runs on the watched repository's pull
    /// requests. `None` unless `CI_GITHUB_CHECK_RUNS_ENABLED` is true: Kurbis
    /// writes nothing to someone else's repository by default.
    pub check_runs_token: Option<String>,
}

impl CiConfig {
    pub fn from_env() -> Result<Option<Self>> {
        let github_pat = match env::var("CI_GITHUB_PAT") {
            Ok(token) if token.is_empty() => return Ok(None),
            Ok(token) if token.trim().is_empty() => return Ok(None),
            Ok(token) => token,
            Err(env::VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(error).context("CI_GITHUB_PAT must be valid UTF-8"),
        };
        ensure!(
            !github_pat.is_empty()
                && github_pat.len() <= 512
                && github_pat.bytes().all(|byte| (0x21..=0x7e).contains(&byte)),
            "CI_GITHUB_PAT must be a non-empty visible token"
        );
        let repository = value("CI_REPOSITORY", crate::ci::CI_TARGET_REPOSITORY);
        validate_ci_repository(&repository)?;
        let github_api_base_url = value("CI_GITHUB_API_BASE_URL", "https://api.github.com");
        validate_ci_github_api_base_url(&github_api_base_url)?;
        // Keep the admission key identical to the fingerprint enforced by the
        // host-compiled source preparer.  Do not duplicate the list of trusted
        // patch/plugin inputs here: a second hash implementation can silently
        // admit a build key that the preparer rejects (or vice versa).
        let patch_fingerprint = crate::ci::checked_in_patch_fingerprint();
        let backend = value("CI_BUILDER_BACKEND", "docker");
        let (builder_backend, builder_image, builder_docker_host, toolchain_version) = match backend
            .as_str()
        {
            "docker" => {
                let builder_image = required("CI_BUILDER_IMAGE")?;
                let builder_docker_host = required("CI_BUILDER_DOCKER_HOST")?;
                validate_ci_builder_docker_host(&builder_docker_host)?;
                let configured_rustc = exact_ci_rust_release()?;
                let toolchain_version =
                    crate::ci::execution::toolchain_identity(&builder_image, &configured_rustc)
                        .context(
                            "CI_BUILDER_IMAGE must be digest-pinned and match CI_TOOLCHAIN_VERSION",
                        )?;
                (
                    CiBuilderBackend::Docker,
                    builder_image,
                    builder_docker_host,
                    toolchain_version,
                )
            }
            "bubblewrap" => {
                let configured_rustc = exact_ci_rust_release()?;
                let executable = required_path("CI_BUBBLEWRAP_PATH")?;
                let rootfs = required_path("CI_BUBBLEWRAP_ROOTFS")?;
                let toolchain = required_path("CI_BUBBLEWRAP_TOOLCHAIN")?;
                let workspace_root = required_path("CI_BUBBLEWRAP_WORKSPACE")?;
                let source_preparer = required_path("CI_BUBBLEWRAP_SOURCE_PREPARER")?;
                (
                    CiBuilderBackend::Bubblewrap {
                        executable,
                        rootfs,
                        toolchain,
                        workspace_root,
                        source_preparer,
                    },
                    String::new(),
                    String::new(),
                    configured_rustc,
                )
            }
            "bubblewrap_ssh" => {
                let configured_rustc = exact_ci_rust_release()?;
                let target = required("CI_BUILDER_SSH_TARGET")?;
                ensure!(
                    target.len() <= 255 && !target.bytes().any(|byte| byte.is_ascii_whitespace()),
                    "CI_BUILDER_SSH_TARGET must be a single SSH target"
                );
                let runner = required_path("CI_BUBBLEWRAP_RUNNER")?;
                let executable = required("CI_BUBBLEWRAP_PATH")?;
                let rootfs = required("CI_BUBBLEWRAP_ROOTFS")?;
                let toolchain = required("CI_BUBBLEWRAP_TOOLCHAIN")?;
                let workspace_root = required("CI_BUBBLEWRAP_WORKSPACE")?;
                let source_preparer = required("CI_BUBBLEWRAP_SOURCE_PREPARER")?;
                let runner_string = runner.to_string_lossy().into_owned();
                for (name, value) in [
                    ("CI_BUBBLEWRAP_PATH", &executable),
                    ("CI_BUBBLEWRAP_ROOTFS", &rootfs),
                    ("CI_BUBBLEWRAP_TOOLCHAIN", &toolchain),
                    ("CI_BUBBLEWRAP_WORKSPACE", &workspace_root),
                    ("CI_BUBBLEWRAP_SOURCE_PREPARER", &source_preparer),
                    ("CI_BUBBLEWRAP_RUNNER", &runner_string),
                ] {
                    validate_ci_remote_path(value, name)?;
                }
                (
                    CiBuilderBackend::BubblewrapSsh {
                        target,
                        runner,
                        executable,
                        rootfs,
                        toolchain,
                        workspace_root,
                        source_preparer,
                    },
                    String::new(),
                    String::new(),
                    configured_rustc,
                )
            }
            other => bail!(
                "CI_BUILDER_BACKEND must be docker, bubblewrap, or bubblewrap_ssh, got {other:?}"
            ),
        };
        let build_concurrency = ci_build_concurrency_from_env()?;
        let github_min_request_interval =
            ci_github_duration_ms_from_env("CI_GITHUB_MIN_REQUEST_INTERVAL_MS", 2_000)?;
        let github_rate_limit_max_backoff =
            ci_github_duration_secs_from_env("CI_GITHUB_RATE_LIMIT_MAX_BACKOFF_SECS", 900)?;
        let github_rate_limit_jitter = ci_github_jitter_from_env()?;
        let target_architecture = native_linux_target_architecture()?;
        let plugin_source = PathBuf::from(value("CI_PLUGIN_SOURCE", "plugins/kurbis-control"));
        let plugin_source = if plugin_source.is_absolute() {
            plugin_source
        } else {
            env::current_dir()?.join(plugin_source)
        };
        let check_runs_token = check_runs_token(
            optional_bool("CI_GITHUB_CHECK_RUNS_ENABLED", false)?,
            env::var("CI_GITHUB_CHECK_RUNS_TOKEN").ok(),
        )?;
        let default_branch = value("CI_DEFAULT_BRANCH", "master");
        ensure!(
            !default_branch.trim().is_empty() && default_branch.len() <= 255,
            "CI_DEFAULT_BRANCH must contain 1 to 255 bytes"
        );
        Ok(Some(Self {
            github_pat,
            repository,
            github_api_base_url,
            default_branch,
            patch_fingerprint,
            toolchain_version,
            target_architecture,
            builder_image,
            builder_docker_host,
            builder_backend,
            build_concurrency,
            github_min_request_interval,
            github_rate_limit_max_backoff,
            github_rate_limit_jitter,
            plugin_source,
            check_runs_token,
        }))
    }
}

impl fmt::Debug for CiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CiConfig")
            .field("github_pat", &"[redacted]")
            .field("repository", &self.repository)
            .field("github_api_base_url", &self.github_api_base_url)
            .field("default_branch", &self.default_branch)
            .field("patch_fingerprint", &self.patch_fingerprint)
            .field("toolchain_version", &self.toolchain_version)
            .field("target_architecture", &self.target_architecture)
            .field("builder_image", &self.builder_image)
            .field("builder_docker_host", &self.builder_docker_host)
            .field("builder_backend", &self.builder_backend)
            .field("build_concurrency", &self.build_concurrency)
            .field(
                "github_min_request_interval",
                &self.github_min_request_interval,
            )
            .field(
                "github_rate_limit_max_backoff",
                &self.github_rate_limit_max_backoff,
            )
            .field("github_rate_limit_jitter", &self.github_rate_limit_jitter)
            .field("plugin_source", &self.plugin_source)
            .finish()
    }
}

pub fn ci_build_concurrency_from_env() -> Result<u16> {
    crate::ci::execution::parse_build_concurrency(env::var("CI_BUILD_CONCURRENCY").ok().as_deref())
        .map_err(|error| anyhow::anyhow!(error))
}

fn ci_github_duration_ms_from_env(name: &str, default_ms: u64) -> Result<Duration> {
    let value = match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be an integer number of milliseconds"))?,
        Err(env::VarError::NotPresent) => default_ms,
        Err(error) => return Err(error).context(format!("{name} must be valid UTF-8")),
    };
    ensure!(
        value > 0 && value <= 86_400_000,
        "{name} must be between 1ms and 24h"
    );
    Ok(Duration::from_millis(value))
}

fn ci_github_duration_secs_from_env(name: &str, default_secs: u64) -> Result<Duration> {
    let value = match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be an integer number of seconds"))?,
        Err(env::VarError::NotPresent) => default_secs,
        Err(error) => return Err(error).context(format!("{name} must be valid UTF-8")),
    };
    ensure!(
        (60..=7 * 24 * 60 * 60).contains(&value),
        "{name} must be between 60s and 7d"
    );
    Ok(Duration::from_secs(value))
}

fn ci_github_jitter_from_env() -> Result<f64> {
    let value = match env::var("CI_GITHUB_RATE_LIMIT_JITTER") {
        Ok(value) => value
            .parse::<f64>()
            .context("CI_GITHUB_RATE_LIMIT_JITTER must be a number")?,
        Err(env::VarError::NotPresent) => 0.2,
        Err(error) => return Err(error).context("CI_GITHUB_RATE_LIMIT_JITTER must be valid UTF-8"),
    };
    ensure!(
        value.is_finite() && (0.0..=1.0).contains(&value),
        "CI_GITHUB_RATE_LIMIT_JITTER must be between 0 and 1"
    );
    Ok(value)
}

fn exact_ci_rust_release() -> Result<String> {
    let configured_rustc = required("CI_TOOLCHAIN_VERSION")?;
    ensure!(
        configured_rustc.len() <= 32
            && configured_rustc.split('.').count() == 3
            && configured_rustc
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
        "CI_TOOLCHAIN_VERSION must be an exact Rust release such as 1.98.1"
    );
    Ok(configured_rustc)
}

fn required_path(name: &str) -> Result<PathBuf> {
    let value = required(name)?;
    ensure!(!value.contains('\0'), "{name} must not contain NUL bytes");
    Ok(PathBuf::from(value))
}

pub fn validate_ci_repository(value: &str) -> Result<()> {
    ensure!(
        value == crate::ci::CI_TARGET_REPOSITORY,
        "CI_REPOSITORY is fixed to {}",
        crate::ci::CI_TARGET_REPOSITORY
    );
    Ok(())
}

/// Validate an absolute path passed to the checked-in remote Bubblewrap runner.
///
/// These values are interpolated into an OpenSSH remote command, so accepting
/// only a small portable path grammar keeps operator configuration from
/// becoming shell syntax. The runner path is checked separately even though it
/// is represented as a PathBuf in the config enum.
pub fn validate_ci_remote_path(value: &str, name: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 512
            && value.starts_with('/')
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte)),
        "{name} must be an absolute remote path containing only [A-Za-z0-9/._-]"
    );
    Ok(())
}

pub fn validate_ci_github_api_base_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("CI_GITHUB_API_BASE_URL must be a valid URL")?;
    ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.scheme(), "https" | "http"),
        "CI_GITHUB_API_BASE_URL must be an HTTP(S) URL without credentials or query"
    );
    if url.scheme() == "http" {
        ensure!(
            matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
            "plain HTTP CI_GITHUB_API_BASE_URL is allowed only for loopback fixtures"
        );
    }
    Ok(())
}

pub fn validate_ci_builder_docker_host(value: &str) -> Result<()> {
    let host = reqwest::Url::parse(value)
        .context("CI_BUILDER_DOCKER_HOST must be a valid SSH Docker host")?;
    ensure!(
        host.scheme() == "ssh"
            && host.host_str().is_some()
            && !host.username().is_empty()
            && host.password().is_none()
            && matches!(host.path(), "" | "/")
            && host.query().is_none()
            && host.fragment().is_none(),
        "CI_BUILDER_DOCKER_HOST must use ssh://user@host[:port]"
    );
    Ok(())
}

/// The token for GitHub Check Runs, which are posted only on explicit opt-in.
fn check_runs_token(enabled: bool, token: Option<String>) -> Result<Option<String>> {
    if !enabled {
        return Ok(None);
    }
    let token = token.unwrap_or_default();
    ensure!(
        !token.is_empty()
            && token.len() <= 512
            && token.bytes().all(|byte| (0x21..=0x7e).contains(&byte)),
        "CI_GITHUB_CHECK_RUNS_ENABLED=true requires CI_GITHUB_CHECK_RUNS_TOKEN to be a non-empty visible token"
    );
    Ok(Some(token))
}

fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be a boolean (true/false)"),
    }
}

fn optional_bool(name: &str, default: bool) -> Result<bool> {
    match env::var(name) {
        Ok(value) => parse_bool(name, &value),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error).context(format!("{name} must be valid UTF-8")),
    }
}

fn native_linux_target_architecture() -> Result<String> {
    ensure!(
        cfg!(target_os = "linux"),
        "the CI control plane and rootless builder require a Linux native host"
    );
    let architecture = match env::consts::ARCH {
        "x86_64" => "x86_64-linux",
        "aarch64" => "aarch64-linux",
        other => bail!("unsupported native CI Linux architecture {other}"),
    };
    Ok(architecture.to_owned())
}

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub google: OAuthProviderConfig,
    pub discord: OAuthProviderConfig,
}

#[derive(Clone)]
pub struct OAuthProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub scopes: String,
}

impl fmt::Debug for OAuthProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthProviderConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[redacted]")
            .field("authorize_url", &self.authorize_url)
            .field("token_url", &self.token_url)
            .field("userinfo_url", &self.userinfo_url)
            .field("scopes", &self.scopes)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Google,
    Discord,
}

impl OAuthProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Discord => "discord",
        }
    }
}

impl OAuthConfig {
    pub fn provider(&self, provider: OAuthProvider) -> &OAuthProviderConfig {
        match provider {
            OAuthProvider::Google => &self.google,
            OAuthProvider::Discord => &self.discord,
        }
    }
}

pub(crate) fn https_public_base_url(value: &str) -> String {
    value.strip_prefix("http://").map_or_else(
        || value.trim_end_matches('/').to_owned(),
        |rest| format!("https://{}", rest.trim_end_matches('/')),
    )
}

impl ControlPlaneConfig {
    /// Returns the public origin using HTTPS for links that Minecraft must open.
    pub fn https_public_base_url(&self) -> String {
        https_public_base_url(&self.public_base_url)
    }

    pub fn from_env() -> Result<Self> {
        let ci_github_webhook_enabled = optional_bool("CI_GITHUB_WEBHOOK_ENABLED", false)?;
        let ci_github_webhook_secret = match env::var("CI_GITHUB_WEBHOOK_SECRET") {
            Ok(secret)
                if (32..=512).contains(&secret.len())
                    && secret.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) =>
            {
                Some(SecretString::new(secret))
            }
            Ok(_) => bail!("CI_GITHUB_WEBHOOK_SECRET must contain 32 to 512 visible ASCII bytes"),
            Err(env::VarError::NotPresent) if !ci_github_webhook_enabled => None,
            Err(env::VarError::NotPresent) => {
                bail!("CI_GITHUB_WEBHOOK_SECRET is required when CI_GITHUB_WEBHOOK_ENABLED=true")
            }
            Err(error) => {
                return Err(error).context("CI_GITHUB_WEBHOOK_SECRET must be valid UTF-8");
            }
        };
        Ok(Self {
            common: CommonConfig::from_env()?,
            listen: value("CONTROL_PLANE_LISTEN", "0.0.0.0:8080").parse()?,
            public_base_url: required("PUBLIC_BASE_URL")?,
            root_domain: required("ROOT_DOMAIN")?,
            internal_token: required("INTERNAL_TOKEN")?,
            ci_github_webhook_enabled,
            ci_github_webhook_secret,
            oauth: OAuthConfig {
                google: oauth_provider(
                    "GOOGLE_OAUTH",
                    "https://accounts.google.com/o/oauth2/v2/auth",
                    "https://oauth2.googleapis.com/token",
                    "https://openidconnect.googleapis.com/v1/userinfo",
                    "openid email profile",
                )?,
                discord: oauth_provider(
                    "DISCORD_OAUTH",
                    "https://discord.com/oauth2/authorize",
                    "https://discord.com/api/oauth2/token",
                    "https://discord.com/api/v10/users/@me",
                    "identify email",
                )?,
            },
        })
    }
}

#[derive(Clone)]
pub struct WorkerConfig {
    pub id: uuid::Uuid,
    pub name: String,
    pub address: String,
    pub signal_address: IpAddr,
    pub gateway_address: Option<IpAddr>,
    pub gateway_bedrock_ip: Option<std::net::Ipv4Addr>,
    pub gateway_bedrock_port: Option<u16>,
    pub root: PathBuf,
    pub storage_mode: StorageMode,
    pub runtime: RuntimeConfig,
    pub total_memory_bytes: i64,
    pub total_disk_bytes: i64,
    pub total_cpu_millis: i64,
    pub port_min: i32,
    pub port_max: i32,
    pub host_control_token: String,
    pub control_plane_url: String,
    pub runtime_api_proxy_upstream: String,
    pub runtime_api_proxy_egress_network: String,
    pub runtime_api_proxy_image: String,
    pub public_base_url: String,
    pub api_token: String,
    pub allow_insecure_local_control_plane_http: bool,
    pub plugin_wasm: Option<PathBuf>,
    pub snapshot_interval: Duration,
    pub idle_timeout: Duration,
}

impl WorkerConfig {
    pub fn from_env() -> Result<Self> {
        let runtime = RuntimeConfig::from_env()?;
        let native = matches!(runtime, RuntimeConfig::Native { .. });
        let id: uuid::Uuid = required("WORKER_ID")?
            .parse()
            .context("invalid WORKER_ID")?;
        let signal_address = required("WORKER_SIGNAL_ADDRESS")?;
        ensure!(
            crate::model::is_private_signal_address(&signal_address),
            "WORKER_SIGNAL_ADDRESS must be a private worker address"
        );
        let signal_address = signal_address.parse::<IpAddr>()?;
        let worker_address = required("WORKER_ADDRESS")?;
        let worker_ip = worker_address
            .parse::<IpAddr>()
            .context("WORKER_ADDRESS must be an IP address")?;
        let gateway_address = match env::var("WORKER_GATEWAY_ADDRESS") {
            Ok(address) => {
                ensure!(
                    crate::model::is_gateway_source_address(&address),
                    "WORKER_GATEWAY_ADDRESS must be a concrete unicast Gateway source IP"
                );
                let address = address.parse::<IpAddr>()?;
                ensure!(
                    !address.is_unspecified()
                        && address.is_ipv4() == signal_address.is_ipv4()
                        && address.is_ipv4() == worker_ip.is_ipv4(),
                    "WORKER_GATEWAY_ADDRESS must be a concrete address in the worker ingress IP family"
                );
                Some(address)
            }
            Err(env::VarError::NotPresent) if native => None,
            Err(env::VarError::NotPresent) => {
                bail!("WORKER_GATEWAY_ADDRESS is required for Docker workloads")
            }
            Err(error) => return Err(error).context("WORKER_GATEWAY_ADDRESS must be UTF-8"),
        };
        let (gateway_bedrock_ip, gateway_bedrock_port) = match (
            env::var("WORKER_GATEWAY_BEDROCK_IP"),
            env::var("WORKER_GATEWAY_BEDROCK_PORT"),
        ) {
            (Ok(address), Ok(port)) => {
                let address: std::net::Ipv4Addr = address
                    .parse()
                    .context("WORKER_GATEWAY_BEDROCK_IP must be IPv4")?;
                ensure!(
                    crate::model::is_gateway_source_address(&address.to_string()),
                    "WORKER_GATEWAY_BEDROCK_IP must be a concrete unicast Gateway public IPv4 address"
                );
                let port = port
                    .parse::<u16>()
                    .context("WORKER_GATEWAY_BEDROCK_PORT must be a valid port")?;
                ensure!(
                    port > 1024,
                    "WORKER_GATEWAY_BEDROCK_PORT must be an unprivileged port"
                );
                (Some(address), Some(port))
            }
            (Err(env::VarError::NotPresent), Err(env::VarError::NotPresent)) if native => {
                (None, None)
            }
            (Err(env::VarError::NotPresent), Err(env::VarError::NotPresent)) => {
                bail!(
                    "Docker workloads require WORKER_GATEWAY_BEDROCK_IP and WORKER_GATEWAY_BEDROCK_PORT"
                )
            }
            (Err(env::VarError::NotPresent), _) | (_, Err(env::VarError::NotPresent)) => {
                bail!(
                    "WORKER_GATEWAY_BEDROCK_IP and WORKER_GATEWAY_BEDROCK_PORT must be configured together"
                )
            }
            (Err(error), _) | (_, Err(error)) => {
                return Err(error).context("WORKER_GATEWAY_BEDROCK_IP/PORT must be valid UTF-8");
            }
        };
        let control_plane_url = required("CONTROL_PLANE_URL")?;
        let runtime_api_proxy_upstream =
            value("WORKER_RUNTIME_API_PROXY_UPSTREAM", &control_plane_url);
        let runtime_api_proxy_egress_network = value(
            "WORKER_RUNTIME_API_EGRESS_NETWORK",
            "kurbis-hoster_runtime-api-egress",
        );
        let runtime_api_proxy_image = value("WORKER_RUNTIME_API_PROXY_IMAGE", "melon-hoster:local");
        let native_root = format!("data/native-worker-{id}");
        let root = PathBuf::from(value(
            "WORKER_ROOT",
            if native {
                &native_root
            } else {
                "/var/lib/kurbis"
            },
        ));
        let root = if root.is_absolute() {
            root
        } else {
            env::current_dir()?.join(root)
        };
        let plugin_profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        let plugin_wasm = match env::var_os("KURBIS_CONTROL_PLUGIN_WASM") {
            Some(path) => {
                let path = PathBuf::from(path);
                Some(if path.is_absolute() {
                    path
                } else {
                    env::current_dir()?.join(path)
                })
            }
            None if native => Some(
                env::current_dir()?.join(
                    PathBuf::from("plugins/kurbis-control/target/wasm32-wasip2")
                        .join(plugin_profile)
                        .join("kurbis_control.wasm"),
                ),
            ),
            None => None,
        };
        Ok(Self {
            id,
            name: required("WORKER_NAME")?,
            address: worker_address,
            signal_address,
            gateway_address,
            gateway_bedrock_ip,
            gateway_bedrock_port,
            root,
            storage_mode: StorageMode::from_env(native)?,
            runtime,
            total_memory_bytes: gib_env("WORKER_MEMORY_GIB", 16)?,
            total_disk_bytes: gib_env("WORKER_DISK_GIB", 100)?,
            total_cpu_millis: value(
                "WORKER_CPU_MILLIS",
                &(std::thread::available_parallelism()
                    .map(|cores| cores.get() as i64)
                    .unwrap_or(1)
                    * 1000)
                    .to_string(),
            )
            .parse()?,
            port_min: value("WORKER_PORT_MIN", "20000").parse()?,
            port_max: value("WORKER_PORT_MAX", "40000").parse()?,
            host_control_token: required("PUMPKIN_HOST_CONTROL_TOKEN")?,
            control_plane_url,
            runtime_api_proxy_upstream,
            runtime_api_proxy_egress_network,
            runtime_api_proxy_image,
            public_base_url: required("PUBLIC_BASE_URL")?,
            api_token: required("WORKER_API_TOKEN")?,
            allow_insecure_local_control_plane_http: value(
                "WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP",
                "false",
            )
            .parse()?,
            plugin_wasm,
            snapshot_interval: Duration::from_secs(
                value("SNAPSHOT_INTERVAL_SECONDS", "300").parse()?,
            ),
            idle_timeout: Duration::from_secs(value("IDLE_TIMEOUT_SECONDS", "300").parse()?),
        })
    }
}

#[derive(Debug, Clone)]
pub enum RuntimeConfig {
    Docker { image: String },
    Native { binary: PathBuf },
}

impl RuntimeConfig {
    fn from_env() -> Result<Self> {
        match value("WORKER_RUNTIME", "docker").as_str() {
            "docker" => Ok(Self::Docker {
                image: value("PUMPKIN_IMAGE", "melon-pumpkin:host-control"),
            }),
            "native"
                if cfg!(debug_assertions)
                    && value("KURBIS_ALLOW_UNSANDBOXED_NATIVE_DEV", "false") == "true" =>
            {
                Ok(Self::Native {
                    binary: PathBuf::from(value(
                        "PUMPKIN_BINARY",
                        "output/native-pumpkin/target/debug/pumpkin",
                    )),
                })
            }
            "native" => bail!(
                "native Pumpkin is an unsandboxed development fixture; it requires a debug build and KURBIS_ALLOW_UNSANDBOXED_NATIVE_DEV=true"
            ),
            other => bail!("unsupported WORKER_RUNTIME {other:?}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageMode {
    LoopbackExt4,
    Directory,
}

impl StorageMode {
    fn from_env(native: bool) -> Result<Self> {
        match value(
            "WORKER_STORAGE_MODE",
            if native { "directory" } else { "loopback_ext4" },
        )
        .as_str()
        {
            "loopback_ext4" => Ok(Self::LoopbackExt4),
            "directory" => Ok(Self::Directory),
            other => bail!("unsupported WORKER_STORAGE_MODE {other:?}"),
        }
    }
}

#[derive(Clone)]
pub struct GatewayConfig {
    pub common: CommonConfig,
    pub java_listen: SocketAddr,
    pub java_public_port: u16,
    pub bedrock_listen: SocketAddr,
    pub bedrock_public_address: std::net::Ipv4Addr,
    pub bedrock_public_port: u16,
    pub bedrock_tls_certificate: PathBuf,
    pub bedrock_tls_private_key: PathBuf,
    pub waiting_java_address: SocketAddr,
    pub waiting_bedrock_address: SocketAddr,
    pub waiting_signal_address: SocketAddr,
    pub waiting_control_socket: PathBuf,
    pub waiting_control_token: String,
    pub control_plane_url: String,
    pub internal_token: String,
    pub root_domain: String,
    pub wake_timeout: Duration,
    pub allow_offline_waiting_for_tests: bool,
}

impl fmt::Debug for GatewayConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayConfig")
            .field("common", &self.common)
            .field("java_listen", &self.java_listen)
            .field("java_public_port", &self.java_public_port)
            .field("bedrock_listen", &self.bedrock_listen)
            .field("bedrock_public_address", &self.bedrock_public_address)
            .field("bedrock_public_port", &self.bedrock_public_port)
            .field("bedrock_tls_certificate", &self.bedrock_tls_certificate)
            .field("bedrock_tls_private_key", &self.bedrock_tls_private_key)
            .field("waiting_java_address", &self.waiting_java_address)
            .field("waiting_bedrock_address", &self.waiting_bedrock_address)
            .field("waiting_signal_address", &self.waiting_signal_address)
            .field("waiting_control_socket", &self.waiting_control_socket)
            .field("waiting_control_token", &"[REDACTED]")
            .field("control_plane_url", &self.control_plane_url)
            .field("internal_token", &"[REDACTED]")
            .field("root_domain", &self.root_domain)
            .field("wake_timeout", &self.wake_timeout)
            .field(
                "allow_offline_waiting_for_tests",
                &self.allow_offline_waiting_for_tests,
            )
            .finish()
    }
}

impl GatewayConfig {
    pub fn from_env() -> Result<Self> {
        let java_public_port = value("JAVA_PUBLIC_PORT", "25565")
            .parse::<u16>()
            .context("invalid JAVA_PUBLIC_PORT")?;
        anyhow::ensure!(
            java_public_port > 1024,
            "JAVA_PUBLIC_PORT must be an unprivileged port"
        );
        let bedrock_public_port: u16 = required("BEDROCK_PUBLIC_PORT")?
            .parse()
            .context("invalid BEDROCK_PUBLIC_PORT")?;
        anyhow::ensure!(
            bedrock_public_port > 1024,
            "BEDROCK_PUBLIC_PORT must be an unprivileged non-zero port"
        );
        let bedrock_public_address: std::net::Ipv4Addr = required("BEDROCK_PUBLIC_IP")?
            .parse()
            .context("BEDROCK_PUBLIC_IP must be IPv4")?;
        anyhow::ensure!(
            !bedrock_public_address.is_unspecified(),
            "BEDROCK_PUBLIC_IP must be a concrete public IPv4 address"
        );
        Ok(Self {
            common: CommonConfig::from_env()?,
            java_listen: value("JAVA_LISTEN", "0.0.0.0:25565").parse()?,
            java_public_port,
            bedrock_listen: value("BEDROCK_LISTEN", "0.0.0.0:19132").parse()?,
            bedrock_public_address,
            bedrock_public_port,
            bedrock_tls_certificate: PathBuf::from(required("BEDROCK_TLS_CERT_FILE")?),
            bedrock_tls_private_key: PathBuf::from(required("BEDROCK_TLS_KEY_FILE")?),
            waiting_java_address: value("WAITING_JAVA_ADDRESS", "127.0.0.1:25566").parse()?,
            waiting_bedrock_address: value("WAITING_BEDROCK_ADDRESS", "127.0.0.1:19133").parse()?,
            waiting_signal_address: value("WAITING_SIGNAL_ADDRESS", "127.0.0.1:19134").parse()?,
            waiting_control_socket: PathBuf::from(value(
                "WAITING_CONTROL_SOCKET",
                "/run/kurbis-gateway/host-control.sock",
            )),
            waiting_control_token: required("PUMPKIN_GATEWAY_CONTROL_TOKEN")?,
            control_plane_url: required("CONTROL_PLANE_URL")?,
            internal_token: required("INTERNAL_TOKEN")?,
            root_domain: required("ROOT_DOMAIN")?,
            wake_timeout: Duration::from_secs(value("WAKE_TIMEOUT_SECONDS", "90").parse()?),
            allow_offline_waiting_for_tests: value("ALLOW_OFFLINE_WAITING_FOR_TESTS", "false")
                .parse()?,
        })
    }
}

#[derive(Clone)]
pub enum ObjectStoreConfig {
    Local {
        root: PathBuf,
    },
    S3 {
        bucket: String,
        region: String,
        endpoint: Option<String>,
        access_key: String,
        secret_key: String,
        allow_http: bool,
    },
}

impl fmt::Debug for ObjectStoreConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local { root } => formatter
                .debug_struct("ObjectStoreConfig::Local")
                .field("root", root)
                .finish(),
            Self::S3 {
                bucket,
                region,
                endpoint,
                allow_http,
                ..
            } => formatter
                .debug_struct("ObjectStoreConfig::S3")
                .field("bucket", bucket)
                .field("region", region)
                .field("endpoint", endpoint)
                .field("access_key", &"[REDACTED]")
                .field("secret_key", &"[REDACTED]")
                .field("allow_http", allow_http)
                .finish(),
        }
    }
}

impl ObjectStoreConfig {
    pub async fn build(&self) -> Result<std::sync::Arc<dyn object_store::ObjectStore>> {
        use object_store::{ObjectStore, aws::AmazonS3Builder, local::LocalFileSystem};

        let store: std::sync::Arc<dyn ObjectStore> = match self {
            Self::Local { root } => {
                tokio::fs::create_dir_all(root).await?;
                std::sync::Arc::new(LocalFileSystem::new_with_prefix(root)?)
            }
            Self::S3 {
                bucket,
                region,
                endpoint,
                access_key,
                secret_key,
                allow_http,
            } => {
                let mut builder = AmazonS3Builder::new()
                    .with_bucket_name(bucket)
                    .with_region(region)
                    .with_access_key_id(access_key)
                    .with_secret_access_key(secret_key)
                    .with_allow_http(*allow_http);
                if let Some(endpoint) = endpoint {
                    builder = builder.with_endpoint(endpoint);
                }
                std::sync::Arc::new(builder.build()?)
            }
        };
        Ok(store)
    }

    pub fn from_env() -> Result<Self> {
        match value("OBJECT_STORE_TYPE", "s3").as_str() {
            "local" => Ok(Self::Local {
                root: PathBuf::from(value(
                    "OBJECT_STORE_LOCAL_ROOT",
                    "/var/lib/kurbis/object-store",
                )),
            }),
            "s3" => Ok(Self::S3 {
                bucket: required("S3_BUCKET")?,
                region: value("S3_REGION", "us-east-1"),
                endpoint: env::var("S3_ENDPOINT").ok().filter(|v| !v.is_empty()),
                access_key: required("S3_ACCESS_KEY")?,
                secret_key: required("S3_SECRET_KEY")?,
                allow_http: value("S3_ALLOW_HTTP", "false").parse()?,
            }),
            other => bail!("unsupported OBJECT_STORE_TYPE {other:?}"),
        }
    }
}

fn required(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("missing environment variable {name}"))
}

fn oauth_provider(
    prefix: &str,
    authorize_url: &str,
    token_url: &str,
    userinfo_url: &str,
    scopes: &str,
) -> Result<OAuthProviderConfig> {
    Ok(OAuthProviderConfig {
        client_id: required(&format!("{prefix}_CLIENT_ID"))?,
        client_secret: required(&format!("{prefix}_CLIENT_SECRET"))?,
        authorize_url: value(&format!("{prefix}_AUTHORIZE_URL"), authorize_url),
        token_url: value(&format!("{prefix}_TOKEN_URL"), token_url),
        userinfo_url: value(&format!("{prefix}_USERINFO_URL"), userinfo_url),
        scopes: scopes.to_owned(),
    })
}

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn gib_env(name: &str, default: i64) -> Result<i64> {
    let gib: i64 = value(name, &default.to_string()).parse()?;
    Ok(gib * 1024 * 1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::{
        CommonConfig, GatewayConfig, ObjectStoreConfig, check_runs_token, https_public_base_url,
        parse_bool, validate_ci_remote_path, validate_ci_repository,
    };

    #[test]
    fn ci_repository_is_explicitly_allowlisted_to_upstream_pumpkin() {
        assert!(validate_ci_repository(crate::ci::CI_TARGET_REPOSITORY).is_ok());
        assert!(validate_ci_repository("legacy-owner/Pumpkin").is_err());
        assert!(validate_ci_repository("Pumpkin-MC/other").is_err());
    }

    #[test]
    fn remote_bubblewrap_paths_reject_shell_syntax() {
        assert!(
            validate_ci_remote_path("/home/ci/kurbis-ci/ci_bubblewrap_runner.py", "runner")
                .is_ok()
        );
        for path in [
            "relative/runner.py",
            "/home/ci/ci runner.py",
            "/home/ci/ci;touch-pwned",
            "/home/ci/ci$(id)",
        ] {
            assert!(
                validate_ci_remote_path(path, "runner").is_err(),
                "accepted {path:?}"
            );
        }
    }

    #[test]
    fn check_runs_need_both_the_explicit_flag_and_a_token() {
        // A token alone never enables writing to the watched repository.
        assert_eq!(check_runs_token(false, Some("token".into())).unwrap(), None);
        assert_eq!(check_runs_token(false, None).unwrap(), None);
        assert_eq!(
            check_runs_token(true, Some("token".into()))
                .unwrap()
                .as_deref(),
            Some("token")
        );
        assert!(check_runs_token(true, None).is_err());
        assert!(check_runs_token(true, Some(String::new())).is_err());
        assert!(check_runs_token(true, Some("has space".into())).is_err());
    }

    #[test]
    fn parse_bool_accepts_explicit_values_and_rejects_ambiguous_values() {
        for value in ["1", "true", "YES", "on"] {
            assert!(parse_bool("KURBIS_TEST_BOOL", value).unwrap());
        }
        for value in ["0", "false", "NO", "off"] {
            assert!(!parse_bool("KURBIS_TEST_BOOL", value).unwrap());
        }
        assert!(parse_bool("KURBIS_TEST_BOOL", "maybe").is_err());
    }

    #[test]
    fn debug_output_redacts_database_and_service_credentials() {
        let common = CommonConfig {
            database_url: "postgres://alice:super-secret@db.example/melon".into(),
        };
        let common_debug = format!("{common:?}");
        assert!(!common_debug.contains("super-secret"));
        assert!(common_debug.contains("REDACTED"));

        let gateway = GatewayConfig {
            common,
            java_listen: "127.0.0.1:25565".parse().unwrap(),
            java_public_port: 25565,
            bedrock_listen: "127.0.0.1:19132".parse().unwrap(),
            bedrock_public_address: "198.51.100.10".parse().unwrap(),
            bedrock_public_port: 19132,
            bedrock_tls_certificate: "/run/cert.pem".into(),
            bedrock_tls_private_key: "/run/key.pem".into(),
            waiting_java_address: "127.0.0.1:25566".parse().unwrap(),
            waiting_bedrock_address: "127.0.0.1:19133".parse().unwrap(),
            waiting_signal_address: "127.0.0.1:19134".parse().unwrap(),
            waiting_control_socket: "/run/gateway.sock".into(),
            waiting_control_token: "gateway-secret".into(),
            control_plane_url: "https://control.example.test".into(),
            internal_token: "internal-secret".into(),
            root_domain: "example.test".into(),
            wake_timeout: std::time::Duration::from_secs(90),
            allow_offline_waiting_for_tests: false,
        };
        let gateway_debug = format!("{gateway:?}");
        assert!(!gateway_debug.contains("gateway-secret"));
        assert!(!gateway_debug.contains("internal-secret"));
        assert!(!gateway_debug.contains("super-secret"));

        let object_store = ObjectStoreConfig::S3 {
            bucket: "bucket".into(),
            region: "us-east-1".into(),
            endpoint: None,
            access_key: "access-secret".into(),
            secret_key: "object-secret".into(),
            allow_http: false,
        };
        let object_store_debug = format!("{object_store:?}");
        assert!(!object_store_debug.contains("access-secret"));
        assert!(!object_store_debug.contains("object-secret"));
    }

    #[test]
    fn minecraft_links_upgrade_http_public_origins_to_https() {
        assert_eq!(
            https_public_base_url("http://panel.example.test/"),
            "https://panel.example.test"
        );
        assert_eq!(
            https_public_base_url("https://panel.example.test/"),
            "https://panel.example.test"
        );
    }
}
