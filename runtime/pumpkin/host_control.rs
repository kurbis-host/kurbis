use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};

use pumpkin_config::whitelist::WhitelistEntry;
use pumpkin_protocol::{
    bedrock::client::transfer::CTransfer as BedrockTransfer, codec::var_int::VarInt,
    java::client::play::CTransfer as JavaTransfer,
};
use pumpkin_util::text::TextComponent;
use serde::Deserialize;
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
    data::SaveJSONConfiguration,
    net::{ClientPlatform, DisconnectReason},
    server::Server,
    stop_server,
};

const MAX_REQUEST_BYTES: u64 = 256 * 1024;
const MIN_TOKEN_BYTES: usize = 32;

#[derive(Deserialize)]
struct Request {
    token: String,
    action: Action,
    player_id: Option<uuid::Uuid>,
    host: Option<String>,
    port: Option<u16>,
    message: Option<String>,
    enabled: Option<bool>,
    whitelist_entries: Option<Vec<WhitelistEntry>>,
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
    Shutdown,
    WhitelistReplace,
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

    loop {
        select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let server = server.clone();
                        let token = token.clone();
                        tokio::spawn(async move {
                            if let Err(error) = handle(stream, &token, &server).await {
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

async fn handle(stream: UnixStream, expected_token: &str, server: &Arc<Server>) -> io::Result<()> {
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
                            && server.advanced_config.networking.bedrock.encryption
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
                        "ready_for_transfer": matches!(player.client.as_ref(), ClientPlatform::Java(_))
                            || player.client_loaded.load(std::sync::atomic::Ordering::Acquire),
                    }))
                })
                .collect::<Vec<_>>();
            write_response(
                &mut write_half,
                json!({"ok": true, "waiting_players": players}),
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
            player.send_system_message_raw(&TextComponent::text(message), true).await;
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
                let mut whitelist = server.data.whitelist_config.write().await;
                whitelist.whitelist = entries;
                whitelist.save();
            }
            server.white_list.store(enabled, Ordering::Release);
            if enabled && server.basic_config.enforce_whitelist {
                let whitelist = server.data.whitelist_config.read().await;
                for player in server.get_all_players() {
                    if whitelist.is_whitelisted(&player.gameprofile) {
                        continue;
                    }
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
        Action::Shutdown => {
            write_response(&mut write_half, json!({"ok": true})).await?;
            stop_server();
            Ok(())
        }
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

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    value: serde_json::Value,
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await
}
