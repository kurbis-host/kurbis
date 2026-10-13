use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use pumpkin_config::op::Op;
use pumpkin_config::whitelist::WhitelistEntry;
use pumpkin_protocol::{
    bedrock::client::transfer::CTransfer as BedrockTransfer, codec::var_int::VarInt,
    java::client::play::CTransfer as JavaTransfer,
};
use pumpkin_util::PermissionLvl;
use pumpkin_util::text::{TextComponent, click::ClickEvent, color::NamedColor};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    select,
};
use tracing::{error, info, warn};

use crate::{
    STOP_INTERRUPT,
    command::CommandSender,
    data::SaveJSONConfiguration,
    net::{ClientPlatform, DisconnectReason},
    server::Server,
    stop_server,
};

const MAX_REQUEST_BYTES: u64 = 256 * 1024;
const MIN_TOKEN_BYTES: usize = 32;
const MAX_COMMAND_BYTES: usize = 512;
const MAX_COMMAND_OUTPUT_LINES: usize = 100;
const MAX_COMMAND_OUTPUT_BYTES: usize = 16 * 1024;
/// The worker writes the owner's installed plugins here, one file per plugin.
/// Pumpkin loads only the files directly inside `plugins/` on start, so these
/// load when the worker asks, after the managed plugin is already running.
const INSTALLED_PLUGIN_DIR: &str = "./plugins/installed";
/// The managed Kurbis plugin, which Pumpkin loads from the top-level plugin
/// directory at startup.
const MANAGED_PLUGIN: &str = "kurbis-control";
const MANAGED_PLUGIN_DATA: &str = "./plugins/data/kurbis-control";
const MAX_PLUGIN_ERROR_BYTES: usize = 1024;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManagedOperatorState {
    player_ids: Vec<uuid::Uuid>,
    #[serde(default)]
    unrelated_ops: Vec<Op>,
}

#[derive(Deserialize)]
struct Request {
    token: String,
    action: Action,
    player_id: Option<uuid::Uuid>,
    host: Option<String>,
    port: Option<u16>,
    message: Option<String>,
    /// Mirror status messages into Java chat in addition to the action bar.
    /// Older gateways omit this field and retain overlay-only behavior.
    chat: Option<bool>,
    /// Opened when a Java player clicks the chat message. Bedrock chat has no
    /// clickable text, so the address is appended to the message instead.
    url: Option<String>,
    enabled: Option<bool>,
    whitelist_entries: Option<Vec<WhitelistEntry>>,
    operators: Option<Vec<WhitelistEntry>>,
    command: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Status,
    Players,
    Message,
    Transfer,
    Kick,
    Save,
    BeginSnapshot,
    EndSnapshot,
    Shutdown,
    WhitelistReplace,
    OperatorsReplace,
    ExecuteCommand,
    Stats,
    SyncPlugins,
}

#[derive(Debug, Serialize)]
struct HostStats {
    cpu_percent: Option<f64>,
    cpu_limit_millis: Option<u64>,
    memory_used_bytes: Option<u64>,
    memory_limit_bytes: Option<u64>,
    disk_used_bytes: Option<u64>,
    disk_limit_bytes: Option<u64>,
    tps: Option<f64>,
}

#[derive(Default)]
struct CpuSample {
    usage_nanos: Option<u64>,
    at: Option<Instant>,
}

impl CpuSample {
    fn observe(
        &mut self,
        usage_nanos: Option<u64>,
        cpu_limit_millis: Option<u64>,
        at: Instant,
    ) -> Option<f64> {
        let value = match (self.usage_nanos, self.at, usage_nanos) {
            (Some(previous_usage), Some(previous_at), Some(current_usage)) => {
                normalize_cpu_percent(
                    previous_usage,
                    current_usage,
                    at.saturating_duration_since(previous_at),
                    cpu_limit_millis,
                )
            }
            _ => None,
        };
        self.usage_nanos = usage_nanos;
        self.at = Some(at);
        value
    }
}

pub fn spawn(server: &Arc<Server>) {
    let Ok(socket_path) = std::env::var("PUMPKIN_HOST_CONTROL_SOCKET") else {
        return;
    };
    let Ok(token) = std::env::var("PUMPKIN_HOST_CONTROL_TOKEN") else {
        error!("PUMPKIN_HOST_CONTROL_SOCKET is set but PUMPKIN_HOST_CONTROL_TOKEN is missing");
        return;
    };
    if token.len() < MIN_TOKEN_BYTES {
        error!("PUMPKIN_HOST_CONTROL_TOKEN must be at least {MIN_TOKEN_BYTES} bytes");
        return;
    }

    let server = server.clone();
    server.clone().spawn_task(async move {
        if let Err(error) = run(PathBuf::from(socket_path), token, server).await {
            error!("Host control stopped: {error}");
        }
    });
}

async fn run(path: PathBuf, token: String, server: Arc<Server>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    match fs::remove_file(&path).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
    info!("Host control is listening on {}", path.display());
    let cpu_sample = Arc::new(std::sync::Mutex::new(CpuSample::default()));

    loop {
        select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let server = server.clone();
                        let token = token.clone();
                        let cpu_sample = cpu_sample.clone();
                        tokio::spawn(async move {
                            if let Err(error) = handle(stream, &token, &server, &cpu_sample).await {
                                warn!("Host control request failed: {error}");
                            }
                        });
                    }
                    Err(error) => warn!("Host control accept failed: {error}"),
                }
            }
            () = STOP_INTERRUPT.cancelled() => break,
        }
    }

    if let Err(error) = fs::remove_file(&path).await
        && error.kind() != io::ErrorKind::NotFound
    {
        warn!(
            "Failed to remove host control socket {}: {error}",
            path.display()
        );
    }
    Ok(())
}

async fn handle(
    stream: UnixStream,
    expected_token: &str,
    server: &Arc<Server>,
    cpu_sample: &Arc<std::sync::Mutex<CpuSample>>,
) -> io::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let reader = BufReader::new(read_half);
    let mut bytes = Vec::new();
    let read = reader
        .take(MAX_REQUEST_BYTES + 1)
        .read_until(b'\n', &mut bytes)
        .await?;
    if read == 0 {
        return Ok(());
    }
    if read as u64 > MAX_REQUEST_BYTES {
        return write_response(
            &mut write_half,
            json!({"ok": false, "error": "request_too_large"}),
        )
        .await;
    }
    let request: Request = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => {
            return write_response(
                &mut write_half,
                json!({"ok": false, "error": "invalid_request"}),
            )
            .await;
        }
    };
    if request.token != expected_token {
        return write_response(
            &mut write_half,
            json!({"ok": false, "error": "unauthorized"}),
        )
        .await;
    }
    if server.snapshot_barrier_active.load(Ordering::Acquire)
        && !matches!(
            &request.action,
            Action::Status
                | Action::Players
                | Action::Stats
                | Action::EndSnapshot
                | Action::Shutdown
        )
    {
        return write_response(
            &mut write_half,
            json!({"ok": false, "error": "snapshot_barrier_active"}),
        )
        .await;
    }

    match request.action {
        Action::Status => {
            write_response(
                &mut write_half,
                json!({"ok": true, "players": server.get_player_count(),
                    "activity_revision": server.player_activity_revision.load(std::sync::atomic::Ordering::Acquire),
                    "online_authentication": {
                        "java": server.advanced_config.networking.java.online_mode
                            && server.advanced_config.networking.java.encryption,
                        "bedrock": server.advanced_config.networking.bedrock.online_mode
                            && server.advanced_config.networking.bedrock.authentication.enabled,
                    }}),
            )
            .await
        }
        Action::Players => {
            let players = server
                .get_all_players()
                .into_iter()
                .filter_map(|player| {
                    let (edition, server_address) = match player.client.as_ref() {
                        ClientPlatform::Java(client) => {
                            ("java", Some(client.server_address.clone()))
                        }
                        ClientPlatform::Bedrock(client) => (
                            "bedrock",
                            client
                                .client_data
                                .load_full()
                                .as_ref()
                                .as_ref()
                                .map(|data| data.server_address.clone()),
                        ),
                    };
                    let server_address = server_address?;
                    let connection_id = player.connection_id;
                    let verified_identity = match player.client.as_ref() {
                        ClientPlatform::Java(_client)
                            if server.advanced_config.networking.java.online_mode
                                && server.advanced_config.networking.java.encryption =>
                        {
                            Some(json!({"kind":"java", "uuid":player.gameprofile.id}))
                        }
                        ClientPlatform::Bedrock(client) => client
                            .verified_xuid
                            .load_full()
                            .as_ref()
                            .as_ref()
                            .map(|xuid| json!({"kind":"bedrock", "xuid":xuid})),
                        _ => None,
                    };
                    Some(json!({
                        "connection_id": connection_id,
                        "id": player.gameprofile.id,
                        "name": player.gameprofile.name,
                        "edition": edition,
                        "server_address": server_address,
                        "verified_identity": verified_identity,
                        "ready_for_transfer": player.has_client_loaded(),
                    }))
                })
                .collect::<Vec<_>>();
            write_response(
                &mut write_half,
                json!({"ok": true, "waiting_players": players}),
            )
            .await
        }
        Action::Stats => {
            write_response(
                &mut write_half,
                json!({"ok": true, "stats": sample_host_stats(server, cpu_sample)}),
            )
            .await
        }
        Action::Message => {
            let Some(player) = requested_player(&request, server) else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "player_not_found"}),
                )
                .await;
            };
            let Some(message) = request.message.filter(|value| value.len() <= 512) else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "invalid_message"}),
                )
                .await;
            };
            let java = matches!(player.client.as_ref(), ClientPlatform::Java(_));
            let url = request
                .url
                .filter(|url| url.len() <= 512 && url.starts_with("https://"));
            match url {
                Some(url) if java => {
                    let link = TextComponent::text(message)
                        .color_named(NamedColor::Aqua)
                        .underlined()
                        .click_event(ClickEvent::OpenUrl { url: url.into() });
                    player.send_system_message_raw(&link, false);
                }
                Some(url) => {
                    let text = TextComponent::text(format!("{message} {url}"));
                    player.send_system_message_raw(&text, true);
                }
                None => {
                    let text = TextComponent::text(message);
                    player.send_system_message_raw(&text, true);
                    if request.chat.unwrap_or(false) && java {
                        player.send_system_message_raw(&text, false);
                    }
                }
            }
            write_response(&mut write_half, json!({"ok": true})).await
        }
        Action::Transfer => {
            let Some(player) = requested_player(&request, server) else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "player_not_found"}),
                )
                .await;
            };
            let Some(host) = request
                .host
                .as_deref()
                .filter(|value| !value.is_empty() && value.len() <= 255)
            else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "invalid_host"}),
                )
                .await;
            };
            let Some(port) = request.port.filter(|port| *port != 0) else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "invalid_port"}),
                )
                .await;
            };
            let java = JavaTransfer::new(host, VarInt(i32::from(port)));
            let bedrock = BedrockTransfer::new(host.to_owned(), port, false);
            player
                .client
                .send_packet_now_editioned(&java, &bedrock)
                .await;
            write_response(&mut write_half, json!({"ok": true})).await
        }
        Action::Kick => {
            let Some(player) = requested_player(&request, server) else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "player_not_found"}),
                )
                .await;
            };
            let message = request
                .message
                .filter(|value| value.len() <= 512)
                .unwrap_or_else(|| "Server unavailable".to_owned());
            player
                .client
                .kick(DisconnectReason::Kicked, TextComponent::text(message))
                .await;
            write_response(&mut write_half, json!({"ok": true})).await
        }
        Action::WhitelistReplace => {
            let Some(enabled) = request.enabled else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "missing_whitelist_enabled"}),
                )
                .await;
            };
            let Some(entries) = request.whitelist_entries else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "missing_whitelist_entries"}),
                )
                .await;
            };
            {
                let mut whitelist = server
                    .data
                    .whitelist_config
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                whitelist.whitelist = entries;
                whitelist.save();
            }
            server.white_list.store(enabled, Ordering::Release);
            if enabled && server.basic_config.enforce_whitelist {
                let to_kick = {
                    let whitelist = server
                        .data
                        .whitelist_config
                        .read()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    server
                        .get_all_players()
                        .into_iter()
                        .filter(|player| !whitelist.is_whitelisted(&player.gameprofile))
                        .collect::<Vec<_>>()
                };
                for player in to_kick {
                    player
                        .client
                        .kick(
                            DisconnectReason::Kicked,
                            TextComponent::text("You are not on this server's whitelist."),
                        )
                        .await;
                }
            }
            write_response(&mut write_half, json!({"ok": true})).await
        }
        Action::OperatorsReplace => {
            let Some(owners) = request.operators else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "missing_operators"}),
                )
                .await;
            };
            let state_path = std::env::current_dir()
                .map(|root| root.join("plugins/data/kurbis-control/operator-config.json"));
            let mut managed_state = match state_path.as_ref() {
                Ok(path) => match read_managed_operator_state(path) {
                    Ok(state) => state,
                    Err(error) => {
                        warn!("Managed operator metadata is invalid: {error}");
                        return write_response(
                            &mut write_half,
                            json!({"ok": false, "error": "invalid_operator_state"}),
                        )
                        .await;
                    }
                },
                Err(error) => {
                    warn!("Managed operator storage path is unavailable: {error}");
                    return write_response(
                        &mut write_half,
                        json!({"ok": false, "error": "invalid_operator_state"}),
                    )
                    .await;
                }
            };
            let desired_ids = owners.iter().map(|owner| owner.uuid).collect::<Vec<_>>();
            let desired_set = desired_ids.iter().copied().collect::<std::collections::HashSet<_>>();
            {
                let mut config = server
                    .data
                    .operator_config
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let previously_managed = managed_state
                    .player_ids
                    .iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>();
                config.ops.retain(|op| !previously_managed.contains(&op.uuid));
                for operator in &owners {
                    if let Some(existing) = config.ops.iter_mut().find(|op| op.uuid == operator.uuid) {
                        existing.name = operator.name.clone();
                        existing.level = PermissionLvl::Four;
                        existing.bypasses_player_limit = true;
                    } else {
                        config.ops.push(Op::new(
                            operator.uuid,
                            operator.name.clone(),
                            PermissionLvl::Four,
                            true,
                        ));
                    }
                }
                config.save();
                managed_state.unrelated_ops = config
                    .ops
                    .iter()
                    .filter(|op| !desired_set.contains(&op.uuid))
                    .cloned()
                    .collect();
                for player in server.get_all_players() {
                    let level = config
                        .ops
                        .iter()
                        .find(|operator| operator.uuid == player.gameprofile.id)
                        .map(|operator| operator.level)
                        .unwrap_or(PermissionLvl::Zero);
                    player.set_permission_lvl(server, level, &server.command_dispatcher.load());
                }
            }
            managed_state.player_ids = desired_ids;
            let persisted = state_path
                .map_err(|error| error.to_string())
                .and_then(|path| {
                    write_managed_operator_state(&path, &managed_state)
                        .map_err(|error| error.to_string())
                });
            if let Err(error) = persisted {
                warn!("Could not persist managed operator metadata: {error}");
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "operator_state_persist_failed"}),
                )
                .await;
            }
            write_response(&mut write_half, json!({"ok": true})).await
        }
        Action::ExecuteCommand => {
            let Some(command) = request.command.filter(|command| {
                !command.trim().is_empty()
                    && command.len() <= MAX_COMMAND_BYTES
                    && !command.chars().any(char::is_control)
            }) else {
                return write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "invalid_command"}),
                )
                .await;
            };
            let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sender = CommandSender::HostControl(output.clone());
            let source = sender.into_source(server);
            server
                .command_dispatcher
                .load()
                .handle_command(&source, command.trim());
            let mut output = output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            output.truncate(MAX_COMMAND_OUTPUT_LINES);
            let mut bytes = 0;
            output.retain(|line| {
                if line.len() > 4_096
                    || line.len() > MAX_COMMAND_OUTPUT_BYTES.saturating_sub(bytes)
                {
                    return false;
                }
                bytes += line.len();
                true
            });
            write_response(
                &mut write_half,
                json!({"ok": true, "output": output}),
            )
            .await
        }
        Action::SyncPlugins => {
            let plugins = sync_installed_plugins(server).await;
            write_response(&mut write_half, json!({"ok": true, "plugins": plugins})).await
        }
        Action::Save => match server.save_all().await {
            Ok(()) => write_response(&mut write_half, json!({"ok": true})).await,
            Err(error) => {
                warn!("Host control save failed: {error}");
                write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "save_failed"}),
                )
                .await
            }
        },
        Action::BeginSnapshot => match server.begin_snapshot_barrier().await {
            Ok(()) => write_response(&mut write_half, json!({"ok": true})).await,
            Err(error) => {
                warn!("Host control snapshot barrier failed: {error}");
                write_response(
                    &mut write_half,
                    json!({"ok": false, "error": "snapshot_barrier_failed"}),
                )
                .await
            }
        },
        Action::EndSnapshot => {
            server.end_snapshot_barrier();
            write_response(&mut write_half, json!({"ok": true})).await
        }
        Action::Shutdown => {
            write_response(&mut write_half, json!({"ok": true})).await?;
            stop_server();
            Ok(())
        }
    }
}

fn sample_host_stats(
    server: &Arc<Server>,
    cpu_sample: &Arc<std::sync::Mutex<CpuSample>>,
) -> HostStats {
    // Keep reading and recording under one lock. Concurrent stats requests can
    // otherwise read in order but update the baseline out of order, producing
    // a false counter reset or an interval that does not match its sample.
    let (_cpu_usage_nanos, cpu_limit_millis, cpu_percent) = {
        let mut previous = cpu_sample
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (usage, limit) = read_cpu_cgroup();
        let percent = previous.observe(usage, limit, Instant::now());
        (usage, limit, percent)
    };
    let (memory_used_bytes, memory_limit_bytes) = read_memory_cgroup();
    let (disk_used_bytes, disk_limit_bytes) = read_disk_usage();
    let tps = normalize_tps(server.get_tps());
    HostStats {
        cpu_percent,
        cpu_limit_millis,
        memory_used_bytes,
        memory_limit_bytes,
        disk_used_bytes,
        disk_limit_bytes,
        tps,
    }
}

fn read_cpu_cgroup() -> (Option<u64>, Option<u64>) {
    // A cgroup-v2 mount always exposes these files when the controller is
    // available. Treat their presence as authoritative, even when one value
    // is malformed or the quota is unlimited, instead of falling through to
    // unrelated cgroup-v1 paths and manufacturing a partial sample.
    let v2_stat = read_text("/sys/fs/cgroup/cpu.stat");
    let v2_max = read_text("/sys/fs/cgroup/cpu.max");
    if v2_stat.is_some() || v2_max.is_some() {
        let usage = v2_stat
            .as_deref()
            .and_then(|value| parse_key_value(value, "usage_usec"))
            .and_then(|value| value.checked_mul(1_000));
        let limit = v2_max.as_deref().and_then(parse_cpu_max);
        return (usage, limit);
    }

    let usage = read_u64("/sys/fs/cgroup/cpuacct/cpuacct.usage")
        .or_else(|| read_u64("/sys/fs/cgroup/cpu,cpuacct/cpuacct.usage"));
    let quota = read_text("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
        .or_else(|| read_text("/sys/fs/cgroup/cpu,cpuacct/cpu.cfs_quota_us"));
    let period = read_text("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
        .or_else(|| read_text("/sys/fs/cgroup/cpu,cpuacct/cpu.cfs_period_us"));
    let limit = match (quota, period) {
        (Some(quota), Some(period)) => parse_v1_cpu(&quota, &period),
        _ => None,
    };
    (usage, limit)
}

fn read_memory_cgroup() -> (Option<u64>, Option<u64>) {
    let v2_current = read_text("/sys/fs/cgroup/memory.current");
    let v2_limit = read_text("/sys/fs/cgroup/memory.max");
    if v2_current.is_some() || v2_limit.is_some() {
        return (
            v2_current.as_deref().and_then(parse_bytes),
            v2_limit.as_deref().and_then(parse_limit),
        );
    }

    let current = read_u64("/sys/fs/cgroup/memory/memory.usage_in_bytes");
    let limit = read_text("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        .as_deref()
        .and_then(parse_limit);
    (current, limit)
}

fn read_disk_usage() -> (Option<u64>, Option<u64>) {
    let Some(path) = std::env::current_dir().ok() else {
        return (None, None);
    };
    let Ok(bytes) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return (None, None);
    };
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: statvfs initializes the struct on success and the path is a valid C string.
    let result = unsafe { libc::statvfs(bytes.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return (None, None);
    }
    // SAFETY: the successful call above initialized the structure.
    let stat = unsafe { stat.assume_init() };
    let block_size = match (stat.f_frsize as u64, stat.f_bsize as u64) {
        (size, _) if size > 0 => size,
        (_, size) if size > 0 => size,
        _ => return (None, None),
    };
    disk_usage(stat.f_blocks as u64, stat.f_bavail as u64, block_size)
}

fn disk_usage(
    total_blocks: u64,
    available_blocks: u64,
    block_size: u64,
) -> (Option<u64>, Option<u64>) {
    if block_size == 0 {
        return (None, None);
    }
    let total = total_blocks.checked_mul(block_size);
    let available = available_blocks.checked_mul(block_size);
    let used = total
        .zip(available)
        .and_then(|(total, available)| total.checked_sub(available));
    (used, total)
}

fn normalize_cpu_percent(
    previous_usage_nanos: u64,
    current_usage_nanos: u64,
    elapsed: Duration,
    cpu_limit_millis: Option<u64>,
) -> Option<f64> {
    let delta = current_usage_nanos.checked_sub(previous_usage_nanos)?;
    let elapsed_nanos = elapsed.as_nanos();
    let limit_millis = cpu_limit_millis?;
    if elapsed_nanos == 0 || limit_millis == 0 {
        return None;
    }
    let allocation = limit_millis as f64 / 1_000.0;
    let used_cpus = delta as f64 / elapsed_nanos as f64;
    Some((used_cpus / allocation * 100.0).clamp(0.0, 100.0))
}

fn normalize_tps(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value.min(20.0))
}

fn read_text(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}
fn read_u64(path: &str) -> Option<u64> {
    read_text(path)?.parse().ok()
}
fn parse_key_value(value: &str, key: &str) -> Option<u64> {
    value.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some(key))
            .then(|| fields.next()?.parse().ok())
            .flatten()
    })
}
fn parse_cpu_max(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let quota = fields.next()?;
    let period = fields.next()?.parse::<u64>().ok()?;
    if quota == "max" || period == 0 {
        return None;
    }
    let quota = quota.parse::<u64>().ok()?;
    if quota == 0 {
        return None;
    }
    Some(quota.checked_mul(1_000)?.checked_div(period)?.max(1))
}
fn parse_v1_cpu(quota: &str, period: &str) -> Option<u64> {
    let quota = quota.parse::<i64>().ok()?;
    let period = period.parse::<u64>().ok()?;
    if quota <= 0 || period == 0 {
        return None;
    }
    Some(
        (quota as u64)
            .checked_mul(1_000)?
            .checked_div(period)?
            .max(1),
    )
}
fn parse_bytes(value: &str) -> Option<u64> {
    value.trim().parse().ok()
}
fn parse_limit(value: &str) -> Option<u64> {
    let value = value.trim();
    if value == "max" {
        return None;
    }
    let parsed = value.parse::<u64>().ok()?;
    // Linux uses a very large sentinel for an unlimited v1 memory limit.
    (parsed < (1_u64 << 60)).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_effective_v2_cpu_quota_without_rounding_sub_milli_limits_to_zero() {
        assert_eq!(parse_cpu_max("50000 100000"), Some(500));
        assert_eq!(parse_cpu_max("200000 100000"), Some(2000));
        assert_eq!(parse_cpu_max("1 100000"), Some(1));
        assert_eq!(parse_cpu_max("max 100000"), None);
        assert_eq!(parse_cpu_max("50000 0"), None);
        assert_eq!(parse_cpu_max("0 100000"), None);
    }

    #[test]
    fn parses_cpu_usage_and_preserves_unavailable_values() {
        assert_eq!(
            parse_key_value("usage_usec 42\nuser_usec 7", "usage_usec"),
            Some(42)
        );
        assert_eq!(parse_key_value("usage_usec nope", "usage_usec"), None);
        assert_eq!(parse_bytes("2048"), Some(2048));
        assert_eq!(parse_bytes("not-a-number"), None);
        assert_eq!(parse_limit("max"), None);
        assert_eq!(parse_limit("9223372036854771712"), None);
        assert_eq!(parse_limit("4096"), Some(4096));
    }

    #[test]
    fn cpu_sample_keeps_usage_and_timestamp_baselines_in_order() {
        let mut sample = CpuSample::default();
        let start = Instant::now();
        assert_eq!(sample.observe(Some(0), Some(500), start), None);
        assert_eq!(
            sample.observe(Some(250_000_000), Some(500), start + Duration::from_secs(1)),
            Some(50.0)
        );
        assert_eq!(
            sample.observe(Some(5), Some(500), start + Duration::from_secs(2)),
            None
        );
        assert_eq!(
            sample.observe(Some(250_000_005), Some(500), start + Duration::from_secs(3)),
            Some(50.0)
        );
        assert_eq!(
            sample.observe(None, None, start + Duration::from_secs(4)),
            None
        );
        assert_eq!(
            sample.observe(Some(500_000_005), Some(500), start + Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn normalizes_cpu_against_effective_quota_and_resets_safely() {
        let one_second = Duration::from_secs(1);
        assert_eq!(
            normalize_cpu_percent(0, 500_000_000, one_second, Some(500)),
            Some(100.0)
        );
        assert_eq!(
            normalize_cpu_percent(0, 250_000_000, one_second, Some(500)),
            Some(50.0)
        );
        assert_eq!(normalize_cpu_percent(10, 9, one_second, Some(500)), None);
        assert_eq!(normalize_cpu_percent(0, 1, Duration::ZERO, Some(500)), None);
        assert_eq!(normalize_cpu_percent(0, 1, one_second, None), None);
    }

    #[test]
    fn calculates_disk_usage_from_statvfs_blocks_without_overflow() {
        assert_eq!(disk_usage(100, 25, 4096), (Some(307_200), Some(409_600)));
        assert_eq!(disk_usage(u64::MAX, 0, 2), (None, None));
        assert_eq!(disk_usage(100, 125, 4096), (None, Some(409_600)));
        assert_eq!(disk_usage(100, 25, 0), (None, None));
    }

    #[test]
    fn caps_tps_to_the_protocol_max_and_omits_invalid_values() {
        assert_eq!(normalize_tps(20.0), Some(20.0));
        assert_eq!(normalize_tps(22.5), Some(20.0));
        assert_eq!(normalize_tps(-1.0), None);
        assert_eq!(normalize_tps(f64::NAN), None);
        assert_eq!(normalize_tps(f64::INFINITY), None);
    }
}

fn requested_player(
    request: &Request,
    server: &Server,
) -> Option<Arc<crate::entity::player::Player>> {
    let id = request.player_id?;
    server
        .get_all_players()
        .into_iter()
        .find(|player| player.gameprofile.id == id)
}

fn read_managed_operator_state(path: &Path) -> io::Result<ManagedOperatorState> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(ManagedOperatorState::default())
        }
        Err(error) => Err(error),
    }
}

fn write_managed_operator_state(path: &Path, state: &ManagedOperatorState) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "operator state path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.pumpkin-tmp");
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)
}

/// Make the loaded installed plugins match the files in their directory:
/// unload the ones whose file is gone, load each new file, and report every
/// file as loaded (with its plugin's name and version) or with its error.
async fn sync_installed_plugins(server: &Arc<Server>) -> Vec<serde_json::Value> {
    let directory = Path::new(INSTALLED_PLUGIN_DIR);
    let mut files = match std::fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "wasm"))
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            warn!("Host control could not list installed plugins: {error}");
            Vec::new()
        }
    };
    files.sort();
    let manager = &server.plugin_manager;
    // A plugin's name selects its data folder, which holds the managed
    // plugin's server credential. The managed plugin claims its name at
    // startup; while it is not running, an installed plugin could claim it.
    if Path::new(MANAGED_PLUGIN_DATA).exists()
        && !manager
            .loaded_plugin_files()
            .iter()
            .any(|(metadata, _)| metadata.name == MANAGED_PLUGIN)
    {
        return files
            .iter()
            .map(|path| {
                json!({
                    "file": plugin_file_name(path),
                    "error": "The Kurbis plugin is not running, so other plugins cannot load.",
                })
            })
            .collect();
    }
    for (metadata, path) in manager.loaded_plugin_files() {
        if path.starts_with(directory) && !files.contains(&path) {
            if let Err(error) = manager.unload_plugin(&metadata.name).await {
                warn!("Host control could not unload plugin {}: {error}", metadata.name);
            }
        }
    }
    let mut report = Vec::with_capacity(files.len());
    for path in files {
        let loaded = manager
            .loaded_plugin_files()
            .into_iter()
            .find(|(_, loaded)| *loaded == path)
            .map(|(metadata, _)| metadata);
        let result = match loaded {
            Some(metadata) => Ok(metadata),
            None => manager.load_plugin_file(server, &path).await,
        };
        let file = plugin_file_name(&path);
        report.push(match result {
            Ok(metadata) => json!({"file": file, "name": metadata.name, "version": metadata.version}),
            Err(mut error) => {
                if error.len() > MAX_PLUGIN_ERROR_BYTES {
                    let mut end = MAX_PLUGIN_ERROR_BYTES;
                    while !error.is_char_boundary(end) {
                        end -= 1;
                    }
                    error.truncate(end);
                }
                json!({"file": file, "error": error})
            }
        });
    }
    report
}

fn plugin_file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    value: serde_json::Value,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await
}
