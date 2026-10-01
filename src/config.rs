use std::{
    env, fmt,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
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

impl ControlPlaneConfig {
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
    pub database_url: String,
    pub id: uuid::Uuid,
    pub name: String,
    pub address: String,
    pub signal_address: IpAddr,
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
            database_url: required("WORKER_DATABASE_URL")?,
            id,
            name: required("WORKER_NAME")?,
            address: required("WORKER_ADDRESS")?,
            signal_address,
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
            control_plane_url: required("CONTROL_PLANE_URL")?,
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
        let default = if cfg!(target_os = "macos") {
            "native"
        } else {
            "docker"
        };
        match value("WORKER_RUNTIME", default).as_str() {
            "docker" => Ok(Self::Docker {
                image: value("PUMPKIN_IMAGE", "melon-pumpkin:host-control"),
            }),
            "native" => Ok(Self::Native {
                binary: PathBuf::from(value(
                    "PUMPKIN_BINARY",
                    "output/native-pumpkin/target/debug/pumpkin",
                )),
            }),
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
