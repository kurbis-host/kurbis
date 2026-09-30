use std::{env, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

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
    pub panel_origin: String,
    pub root_domain: String,
    pub internal_token: String,
    pub oauth: OAuthConfig,
}

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub provider: String,
    pub client_id: String,
    pub client_secret: String,
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub scopes: String,
}

impl ControlPlaneConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            common: CommonConfig::from_env()?,
            listen: value("CONTROL_PLANE_LISTEN", "0.0.0.0:8080").parse()?,
            public_base_url: required("PUBLIC_BASE_URL")?,
            panel_origin: required("PANEL_ORIGIN")?,
            root_domain: required("ROOT_DOMAIN")?,
            internal_token: required("INTERNAL_TOKEN")?,
            oauth: OAuthConfig {
                provider: value("OAUTH_PROVIDER", "oidc"),
                client_id: required("OAUTH_CLIENT_ID")?,
                client_secret: required("OAUTH_CLIENT_SECRET")?,
                authorize_url: required("OAUTH_AUTHORIZE_URL")?,
                token_url: required("OAUTH_TOKEN_URL")?,
                userinfo_url: required("OAUTH_USERINFO_URL")?,
                scopes: value("OAUTH_SCOPES", "openid profile email"),
            },
        })
    }
}

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub common: CommonConfig,
    pub id: uuid::Uuid,
    pub name: String,
    pub address: String,
    pub root: PathBuf,
    pub storage_mode: StorageMode,
    pub pumpkin_image: String,
    pub total_memory_bytes: i64,
    pub total_disk_bytes: i64,
    pub total_cpu_millis: i64,
    pub port_min: i32,
    pub port_max: i32,
    pub host_control_token: String,
    pub snapshot_interval: Duration,
    pub idle_timeout: Duration,
    pub object_store: ObjectStoreConfig,
}

impl WorkerConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            common: CommonConfig::from_env()?,
            id: required("WORKER_ID")?
                .parse()
                .context("invalid WORKER_ID")?,
            name: required("WORKER_NAME")?,
            address: required("WORKER_ADDRESS")?,
            root: PathBuf::from(value("WORKER_ROOT", "/var/lib/kurbis")),
            storage_mode: StorageMode::from_env()?,
            pumpkin_image: value("PUMPKIN_IMAGE", "ghcr.io/pumpkin-mc/pumpkin:nightly"),
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
            snapshot_interval: Duration::from_secs(
                value("SNAPSHOT_INTERVAL_SECONDS", "300").parse()?,
            ),
            idle_timeout: Duration::from_secs(value("IDLE_TIMEOUT_SECONDS", "300").parse()?),
            object_store: ObjectStoreConfig::from_env()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageMode {
    LoopbackExt4,
    Directory,
}

impl StorageMode {
    fn from_env() -> Result<Self> {
        match value("WORKER_STORAGE_MODE", "loopback_ext4").as_str() {
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
    pub waiting_java_address: SocketAddr,
    pub waiting_control_socket: PathBuf,
    pub waiting_control_token: String,
    pub control_plane_url: String,
    pub internal_token: String,
    pub root_domain: String,
    pub wake_timeout: Duration,
}

impl GatewayConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            common: CommonConfig::from_env()?,
            java_listen: value("JAVA_LISTEN", "0.0.0.0:25565").parse()?,
            bedrock_listen: value("BEDROCK_LISTEN", "0.0.0.0:19132").parse()?,
            waiting_java_address: value("WAITING_JAVA_ADDRESS", "127.0.0.1:25566").parse()?,
            waiting_control_socket: PathBuf::from(value(
                "WAITING_CONTROL_SOCKET",
                "/run/kurbis-gateway/host-control.sock",
            )),
            waiting_control_token: required("PUMPKIN_GATEWAY_CONTROL_TOKEN")?,
            control_plane_url: required("CONTROL_PLANE_URL")?,
            internal_token: required("INTERNAL_TOKEN")?,
            root_domain: required("ROOT_DOMAIN")?,
            wake_timeout: Duration::from_secs(value("WAKE_TIMEOUT_SECONDS", "90").parse()?),
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

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn gib_env(name: &str, default: i64) -> Result<i64> {
    let gib: i64 = value(name, &default.to_string()).parse()?;
    Ok(gib * 1024 * 1024 * 1024)
}
