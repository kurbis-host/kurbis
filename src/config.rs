use std::{
    env, fmt,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct CommonConfig {
    pub database_url: String,
}

impl CommonConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: required("DATABASE_URL")?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ControlPlaneConfig {
    pub common: CommonConfig,
    pub listen: SocketAddr,
    pub public_base_url: String,
    pub root_domain: String,
    pub internal_token: String,
    pub oauth: OAuthConfig,
}

#[derive(Clone)]
pub struct CiConfig {
    pub github_pat: String,
    pub repository: String,
    pub default_branch: String,
    pub patch_fingerprint: String,
    pub toolchain_version: String,
    pub target_architecture: String,
    pub builder_image: String,
    pub builder_docker_host: String,
    pub plugin_source: PathBuf,
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
        let repository = value("CI_REPOSITORY", "kurbis-host/Pumpkin");
        ensure!(
            repository == "kurbis-host/Pumpkin",
            "CI_REPOSITORY is fixed to kurbis-host/Pumpkin"
        );
        let patch_fingerprint = checked_in_ci_patch_fingerprint()?;
        let builder_image = required("CI_BUILDER_IMAGE")?;
        let builder_docker_host = required("CI_BUILDER_DOCKER_HOST")?;
        validate_ci_builder_docker_host(&builder_docker_host)?;
        let configured_rustc = required("CI_TOOLCHAIN_VERSION")?;
        ensure!(
            configured_rustc.len() <= 32
                && configured_rustc.split('.').count() == 3
                && configured_rustc
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
            "CI_TOOLCHAIN_VERSION must be an exact Rust release such as 1.98.1"
        );
        let toolchain_version =
            crate::ci::execution::toolchain_identity(&builder_image, &configured_rustc)
                .context("CI_BUILDER_IMAGE must be digest-pinned and match CI_TOOLCHAIN_VERSION")?;
        let target_architecture = native_linux_target_architecture()?;
        let plugin_source = PathBuf::from(value("CI_PLUGIN_SOURCE", "plugins/kurbis-control"));
        let plugin_source = if plugin_source.is_absolute() {
            plugin_source
        } else {
            env::current_dir()?.join(plugin_source)
        };
        let default_branch = value("CI_DEFAULT_BRANCH", "main");
        ensure!(
            !default_branch.trim().is_empty() && default_branch.len() <= 255,
            "CI_DEFAULT_BRANCH must contain 1 to 255 bytes"
        );
        Ok(Some(Self {
            github_pat,
            repository,
            default_branch,
            patch_fingerprint,
            toolchain_version,
            target_architecture,
            builder_image,
            builder_docker_host,
            plugin_source,
        }))
    }
}

impl fmt::Debug for CiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CiConfig")
            .field("github_pat", &"[redacted]")
            .field("repository", &self.repository)
            .field("default_branch", &self.default_branch)
            .field("patch_fingerprint", &self.patch_fingerprint)
            .field("toolchain_version", &self.toolchain_version)
            .field("target_architecture", &self.target_architecture)
            .field("builder_image", &self.builder_image)
            .field("builder_docker_host", &self.builder_docker_host)
            .field("plugin_source", &self.plugin_source)
            .finish()
    }
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

fn checked_in_ci_patch_fingerprint() -> Result<String> {
    let roots = [
        env::var_os("CI_INTEGRATION_SOURCE_ROOT").map(PathBuf::from),
        Some(env::current_dir()?),
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
    ]
    .into_iter()
    .flatten();
    let root = roots
        .into_iter()
        .find(|root| {
            root.join("runtime/pumpkin/host-control.patch").is_file()
                && root
                    .join("runtime/pumpkin/nethernet-gateway.patch")
                    .is_file()
        })
        .context("checked-in Pumpkin integration patches were not found")?;
    let mut hasher = blake3::Hasher::new();
    for (label, relative) in [
        ("pumpkin-revision", "runtime/pumpkin/REVISION"),
        ("host-control.patch", "runtime/pumpkin/host-control.patch"),
        (
            "nethernet-gateway.patch",
            "runtime/pumpkin/nethernet-gateway.patch",
        ),
        ("host_control.rs", "runtime/pumpkin/host_control.rs"),
        (
            "trusted-plugin/Cargo.toml",
            "plugins/kurbis-control/Cargo.toml",
        ),
        (
            "trusted-plugin/Cargo.lock",
            "plugins/kurbis-control/Cargo.lock",
        ),
        (
            "trusted-plugin/src/actions.rs",
            "plugins/kurbis-control/src/actions.rs",
        ),
        (
            "trusted-plugin/src/api.rs",
            "plugins/kurbis-control/src/api.rs",
        ),
        (
            "trusted-plugin/src/chat.rs",
            "plugins/kurbis-control/src/chat.rs",
        ),
        (
            "trusted-plugin/src/command.rs",
            "plugins/kurbis-control/src/command.rs",
        ),
        (
            "trusted-plugin/src/lib.rs",
            "plugins/kurbis-control/src/lib.rs",
        ),
        (
            "trusted-plugin/src/menu.rs",
            "plugins/kurbis-control/src/menu.rs",
        ),
    ] {
        let contents =
            std::fs::read(root.join(relative)).with_context(|| format!("read {relative}"))?;
        hasher.update(&(label.len() as u64).to_be_bytes());
        hasher.update(label.as_bytes());
        hasher.update(&(contents.len() as u64).to_be_bytes());
        hasher.update(&contents);
    }
    Ok(hasher.finalize().to_hex().to_string())
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
        Ok(Self {
            common: CommonConfig::from_env()?,
            listen: value("CONTROL_PLANE_LISTEN", "0.0.0.0:8080").parse()?,
            public_base_url: required("PUBLIC_BASE_URL")?,
            root_domain: required("ROOT_DOMAIN")?,
            internal_token: required("INTERNAL_TOKEN")?,
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

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub common: CommonConfig,
    pub java_listen: SocketAddr,
    pub bedrock_listen: SocketAddr,
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

impl GatewayConfig {
    pub fn from_env() -> Result<Self> {
        let bedrock_public_port: u16 = required("BEDROCK_PUBLIC_PORT")?
            .parse()
            .context("invalid BEDROCK_PUBLIC_PORT")?;
        anyhow::ensure!(
            bedrock_public_port != 0,
            "BEDROCK_PUBLIC_PORT must be non-zero"
        );
        Ok(Self {
            common: CommonConfig::from_env()?,
            java_listen: value("JAVA_LISTEN", "0.0.0.0:25565").parse()?,
            bedrock_listen: value("BEDROCK_LISTEN", "0.0.0.0:19132").parse()?,
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

#[derive(Debug, Clone)]
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
    use super::https_public_base_url;

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
