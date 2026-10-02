use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Parser;
use rand::Rng;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(about = "Provision a uniquely authenticated Kurbis worker")]
struct Args {
    #[arg(long, env = "WORKER_ID")]
    worker_id: Uuid,
    #[arg(long, env = "WORKER_PROVISIONING_DATABASE_URL")]
    database_url: String,
    #[arg(long, env = "WORKER_BUNDLE_PATH")]
    output: Option<PathBuf>,
    #[arg(long)]
    rotate: bool,
}

#[derive(Debug)]
struct WorkerRecord {
    id: Uuid,
    name: String,
    address: String,
    signal_address: String,
    control_plane_url: String,
    public_base_url: String,
    runtime: String,
    storage_mode: String,
    root: String,
    allow_insecure_local_http: bool,
    snapshot_interval_seconds: u64,
    idle_timeout_seconds: u64,
    pumpkin_binary: Option<String>,
    pumpkin_image: Option<String>,
    plugin_wasm: Option<String>,
    total_memory_bytes: i64,
    total_disk_bytes: i64,
    total_cpu_millis: i64,
    port_min: i32,
    port_max: i32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let worker = worker_from_env(args.worker_id)?;
    let admin_options = PgConnectOptions::from_str(&args.database_url)
        .context("invalid worker provisioning database URL")?;
    let worker_api_token = random_secret();
    let host_control_token = random_secret();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(admin_options)
        .await
        .context("connect to the worker provisioning database")?;
    provision_worker(&pool, &worker, &worker_api_token, args.rotate).await?;

    let output = args
        .output
        .unwrap_or_else(|| PathBuf::from(format!("data/worker-bundles/{}.env", worker.id)));
    write_bundle(&output, &worker, &worker_api_token, &host_control_token)?;
    println!("Created worker bundle at {}", output.display());
    Ok(())
}

fn worker_from_env(id: Uuid) -> Result<WorkerRecord> {
    let signal_address = required_env("WORKER_SIGNAL_ADDRESS")?;
    ensure!(
        kurbis_hoster::model::is_private_signal_address(&signal_address),
        "WORKER_SIGNAL_ADDRESS must be a private worker IP address"
    );
    let worker = WorkerRecord {
        id,
        name: required_env("WORKER_NAME")?,
        address: required_env("WORKER_ADDRESS")?,
        signal_address,
        control_plane_url: required_env("WORKER_CONTROL_PLANE_URL")?,
        public_base_url: required_env("PUBLIC_BASE_URL")?,
        runtime: required_env("WORKER_RUNTIME")?,
        storage_mode: required_env("WORKER_STORAGE_MODE")?,
        root: required_env("WORKER_ROOT")?,
        allow_insecure_local_http: env::var("WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP")
            .unwrap_or_else(|_| "false".to_owned())
            .parse()
            .context("invalid WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP")?,
        snapshot_interval_seconds: env::var("SNAPSHOT_INTERVAL_SECONDS")
            .unwrap_or_else(|_| "300".to_owned())
            .parse()
            .context("invalid SNAPSHOT_INTERVAL_SECONDS")?,
        idle_timeout_seconds: env::var("IDLE_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| "300".to_owned())
            .parse()
            .context("invalid IDLE_TIMEOUT_SECONDS")?,
        pumpkin_binary: env::var("PUMPKIN_BINARY").ok(),
        pumpkin_image: env::var("PUMPKIN_IMAGE").ok(),
        plugin_wasm: env::var("KURBIS_CONTROL_PLUGIN_WASM").ok(),
        total_memory_bytes: gib_env("WORKER_MEMORY_GIB", 16)?,
        total_disk_bytes: gib_env("WORKER_DISK_GIB", 100)?,
        total_cpu_millis: env::var("WORKER_CPU_MILLIS")
            .unwrap_or_else(|_| {
                (std::thread::available_parallelism()
                    .map(|cores| cores.get() as i64)
                    .unwrap_or(1)
                    * 1000)
                    .to_string()
            })
            .parse()
            .context("invalid WORKER_CPU_MILLIS")?,
        port_min: env::var("WORKER_PORT_MIN")
            .unwrap_or_else(|_| "20000".to_owned())
            .parse()
            .context("invalid WORKER_PORT_MIN")?,
        port_max: env::var("WORKER_PORT_MAX")
            .unwrap_or_else(|_| "40000".to_owned())
            .parse()
            .context("invalid WORKER_PORT_MAX")?,
    };
    ensure!(
        worker.total_memory_bytes > 0,
        "worker memory must be positive"
    );
    ensure!(worker.total_disk_bytes > 0, "worker disk must be positive");
    ensure!(worker.total_cpu_millis > 0, "worker CPU must be positive");
    ensure!(
        matches!(worker.runtime.as_str(), "native" | "docker"),
        "WORKER_RUNTIME must be native or docker"
    );
    ensure!(
        matches!(worker.storage_mode.as_str(), "directory" | "loopback_ext4"),
        "WORKER_STORAGE_MODE is unsupported"
    );
    ensure!(
        worker.port_min > 1024
            && worker.port_max <= 65535
            && worker.port_max - worker.port_min >= 2,
        "worker port range must include at least three ports"
    );
    Ok(worker)
}

async fn provision_worker(
    pool: &PgPool,
    worker: &WorkerRecord,
    api_token: &str,
    rotate: bool,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(724517, hashtext($1))")
        .bind(worker.id.to_string())
        .execute(&mut *tx)
        .await?;
    // Existing-token operations lock credential -> assignment rows. Rotation
    // follows that order before taking the worker registration row.
    let previous_worker: Option<Uuid> = sqlx::query_scalar(
        "SELECT worker_id FROM worker_credentials WHERE worker_id = $1 FOR UPDATE",
    )
    .bind(worker.id)
    .fetch_optional(&mut *tx)
    .await?;
    match (previous_worker.is_some(), rotate) {
        (true, false) => {
            bail!("worker credentials already exist; pass --rotate to replace them")
        }
        (false, true) => bail!("cannot rotate credentials for a worker that is not provisioned"),
        _ => {}
    }

    // Prevent placement admission from racing the no-active-server check.
    sqlx::query("SELECT id FROM workers WHERE id = $1 FOR UPDATE")
        .bind(worker.id)
        .fetch_optional(&mut *tx)
        .await?;
    let active_placements: i64 =
        sqlx::query_scalar("SELECT count(*) FROM placements WHERE worker_id = $1 AND active")
            .bind(worker.id)
            .fetch_one(&mut *tx)
            .await?;
    ensure!(
        active_placements == 0,
        "stop this worker's hosted servers before provisioning or rotating its credentials"
    );

    sqlx::query(
        "INSERT INTO workers(
             id, name, address, signal_address, total_memory_bytes, total_disk_bytes,
             total_cpu_millis, port_min, port_max
         ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)
         ON CONFLICT(id) DO UPDATE SET
             name = EXCLUDED.name,
             address = EXCLUDED.address,
             signal_address = EXCLUDED.signal_address,
             total_memory_bytes = EXCLUDED.total_memory_bytes,
             total_disk_bytes = EXCLUDED.total_disk_bytes,
             total_cpu_millis = EXCLUDED.total_cpu_millis,
             port_min = EXCLUDED.port_min,
             port_max = EXCLUDED.port_max",
    )
    .bind(worker.id)
    .bind(&worker.name)
    .bind(&worker.address)
    .bind(&worker.signal_address)
    .bind(worker.total_memory_bytes)
    .bind(worker.total_disk_bytes)
    .bind(worker.total_cpu_millis)
    .bind(worker.port_min)
    .bind(worker.port_max)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO worker_credentials(worker_id, api_token_hash)
         VALUES($1, $2)
         ON CONFLICT(worker_id) DO UPDATE SET
             api_token_hash = EXCLUDED.api_token_hash,
             lease_id = NULL,
             lease_expires_at = NULL,
             revoked_at = NULL,
             created_at = now()",
    )
    .bind(worker.id)
    .bind(blake3::hash(api_token.as_bytes()).as_bytes().to_vec())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

fn write_bundle(
    path: &Path,
    worker: &WorkerRecord,
    api_token: &str,
    host_control_token: &str,
) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let temporary = parent.join(format!(".{}.{}.tmp", worker.id, Uuid::new_v4()));
    let mut contents = format!(
        "export WORKER_ID={}\nexport WORKER_NAME={}\nexport WORKER_ADDRESS={}\nexport WORKER_SIGNAL_ADDRESS={}\nexport WORKER_API_TOKEN={}\nexport PUMPKIN_HOST_CONTROL_TOKEN={}\nexport CONTROL_PLANE_URL={}\nexport PUBLIC_BASE_URL={}\nexport WORKER_RUNTIME={}\nexport WORKER_STORAGE_MODE={}\nexport WORKER_ROOT={}\nexport WORKER_ALLOW_INSECURE_LOCAL_CONTROL_PLANE_HTTP={}\nexport WORKER_MEMORY_GIB={}\nexport WORKER_DISK_GIB={}\nexport WORKER_CPU_MILLIS={}\nexport WORKER_PORT_MIN={}\nexport WORKER_PORT_MAX={}\nexport SNAPSHOT_INTERVAL_SECONDS={}\nexport IDLE_TIMEOUT_SECONDS={}\n",
        shell_quote(&worker.id.to_string()),
        shell_quote(&worker.name),
        shell_quote(&worker.address),
        shell_quote(&worker.signal_address),
        shell_quote(api_token),
        shell_quote(host_control_token),
        shell_quote(&worker.control_plane_url),
        shell_quote(&worker.public_base_url),
        shell_quote(&worker.runtime),
        shell_quote(&worker.storage_mode),
        shell_quote(&worker.root),
        worker.allow_insecure_local_http,
        worker.total_memory_bytes / 1024_i64.pow(3),
        worker.total_disk_bytes / 1024_i64.pow(3),
        worker.total_cpu_millis,
        worker.port_min,
        worker.port_max,
        worker.snapshot_interval_seconds,
        worker.idle_timeout_seconds,
    );
    if let Some(binary) = &worker.pumpkin_binary {
        contents.push_str(&format!("export PUMPKIN_BINARY={}\n", shell_quote(binary)));
    }
    if let Some(image) = &worker.pumpkin_image {
        contents.push_str(&format!("export PUMPKIN_IMAGE={}\n", shell_quote(image)));
    }
    if let Some(plugin) = &worker.plugin_wasm {
        contents.push_str(&format!(
            "export KURBIS_CONTROL_PLUGIN_WASM={}\n",
            shell_quote(plugin)
        ));
    }
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Ok(directory) = OpenOptions::new().read(true).open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn required_env(name: &str) -> Result<String> {
    env::var(name).with_context(|| format!("missing environment variable {name}"))
}

fn gib_env(name: &str, default: i64) -> Result<i64> {
    let value = env::var(name).unwrap_or_else(|_| default.to_string());
    let gib: i64 = value.parse().with_context(|| format!("invalid {name}"))?;
    gib.checked_mul(1024_i64.pow(3))
        .with_context(|| format!("{name} is too large"))
}
