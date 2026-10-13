use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};
use uuid::Uuid;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PlayerEdition {
    Java,
    Bedrock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerifiedMinecraftIdentity {
    Java { uuid: Uuid },
    Bedrock { xuid: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WaitingPlayer {
    pub connection_id: Uuid,
    pub id: Uuid,
    pub name: String,
    pub edition: PlayerEdition,
    pub server_address: String,
    pub ready_for_transfer: bool,
    #[serde(default)]
    pub verified_identity: Option<VerifiedMinecraftIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WhitelistEntry {
    #[serde(rename = "uuid")]
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug)]
pub struct PlayerStatus {
    pub players: usize,
    pub activity_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostStats {
    pub cpu_percent: Option<f64>,
    pub cpu_limit_millis: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_limit_bytes: Option<u64>,
    pub tps: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct OnlineAuthentication {
    java: bool,
    bedrock: bool,
}

#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    players: Option<usize>,
    activity_revision: Option<u64>,
    online_authentication: Option<OnlineAuthentication>,
    waiting_players: Option<Vec<WaitingPlayer>>,
    stats: Option<HostStats>,
    output: Option<Vec<String>>,
    error: Option<String>,
}

#[derive(Clone)]
pub struct PumpkinControl {
    token: String,
}

impl PumpkinControl {
    pub fn new(token: String) -> Self {
        Self { token }
    }

    pub async fn status(&self, socket: &Path) -> Result<PlayerStatus> {
        let response = self.request(socket, json!({"action":"status"})).await?;
        Ok(PlayerStatus {
            players: response
                .players
                .context("status response omitted players")?,
            activity_revision: response
                .activity_revision
                .context("status response omitted activity_revision")?,
        })
    }

    pub async fn stats(&self, socket: &Path) -> Result<HostStats> {
        let response = self.request(socket, json!({"action":"stats"})).await?;
        response.stats.context("stats response omitted stats")
    }

    pub async fn online_authentication(&self, socket: &Path) -> Result<(bool, bool)> {
        let response = self.request(socket, json!({"action":"status"})).await?;
        let status = response
            .online_authentication
            .context("status response omitted online_authentication")?;
        Ok((status.java, status.bedrock))
    }

    pub async fn players(&self, socket: &Path) -> Result<Vec<WaitingPlayer>> {
        let response = self.request(socket, json!({"action":"players"})).await?;
        response
            .waiting_players
            .context("players response omitted waiting_players")
    }

    pub async fn replace_whitelist(
        &self,
        socket: &Path,
        enabled: bool,
        entries: &[WhitelistEntry],
    ) -> Result<()> {
        self.request(
            socket,
            json!({
                "action":"whitelist_replace",
                "enabled":enabled,
                "whitelist_entries":entries,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn replace_operators(
        &self,
        socket: &Path,
        operators: &[WhitelistEntry],
    ) -> Result<()> {
        self.request(
            socket,
            json!({"action":"operators_replace", "operators":operators}),
        )
        .await?;
        Ok(())
    }

    /// Keep the lifecycle status in the existing action-bar surface and ask
    /// the runtime to mirror it into Java chat. The runtime decides whether a
    /// second packet is needed for the connected edition, so Bedrock clients do
    /// not receive duplicate system messages.
    pub async fn message(&self, socket: &Path, player_id: Uuid, message: &str) -> Result<()> {
        self.request(
            socket,
            json!({
                "action":"message",
                "player_id":player_id,
                "message":message,
                "chat":true,
            }),
        )
        .await?;
        Ok(())
    }

    /// Send a chat message that opens `url` when a Java player clicks it.
    pub async fn link(
        &self,
        socket: &Path,
        player_id: Uuid,
        message: &str,
        url: &str,
    ) -> Result<()> {
        self.request(
            socket,
            json!({
                "action":"message",
                "player_id":player_id,
                "message":message,
                "url":url,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn transfer(
        &self,
        socket: &Path,
        player_id: Uuid,
        host: &str,
        port: u16,
    ) -> Result<()> {
        self.request(
            socket,
            json!({"action":"transfer", "player_id":player_id, "host":host, "port":port}),
        )
        .await?;
        Ok(())
    }

    pub async fn kick(&self, socket: &Path, player_id: Uuid, message: &str) -> Result<()> {
        self.request(
            socket,
            json!({"action":"kick", "player_id":player_id, "message":message}),
        )
        .await?;
        Ok(())
    }

    pub async fn execute_command(&self, socket: &Path, command: &str) -> Result<Vec<String>> {
        let response = self
            .request(
                socket,
                json!({"action":"execute_command", "command":command}),
            )
            .await?;
        response
            .output
            .context("console command response omitted output")
    }

    pub async fn save(&self, socket: &Path) -> Result<()> {
        self.request(socket, json!({"action":"save"})).await?;
        Ok(())
    }

    pub async fn begin_snapshot_barrier(&self, socket: &Path) -> Result<()> {
        self.request(socket, json!({"action":"begin_snapshot"}))
            .await?;
        Ok(())
    }

    pub async fn end_snapshot_barrier(&self, socket: &Path) -> Result<()> {
        self.request(socket, json!({"action":"end_snapshot"}))
            .await?;
        Ok(())
    }

    pub async fn shutdown(&self, socket: &Path) -> Result<()> {
        self.request(socket, json!({"action":"shutdown"})).await?;
        Ok(())
    }

    async fn request(&self, socket: &Path, mut payload: Value) -> Result<Response> {
        let object = payload
            .as_object_mut()
            .context("host-control request payload must be an object")?;
        object.insert("token".into(), Value::String(self.token.clone()));

        let stream = timeout(IO_TIMEOUT, connect(socket))
            .await
            .context("host-control connection timed out")??;
        let (read_half, mut write_half) = stream.into_split();
        let mut payload = serde_json::to_vec(&payload)?;
        payload.push(b'\n');
        timeout(IO_TIMEOUT, write_half.write_all(&payload))
            .await
            .context("host-control write timed out")??;

        let reader = BufReader::new(read_half);
        let mut bytes = Vec::new();
        let read = timeout(
            IO_TIMEOUT,
            reader
                .take(MAX_RESPONSE_BYTES + 1)
                .read_until(b'\n', &mut bytes),
        )
        .await
        .context("host-control response timed out")??;
        if read == 0 {
            bail!("host-control closed without a response");
        }
        if read as u64 > MAX_RESPONSE_BYTES {
            bail!("host-control response exceeded limit");
        }
        let response: Response = serde_json::from_slice(&bytes)?;
        if !response.ok {
            bail!(
                "host-control request failed: {}",
                response.error.as_deref().unwrap_or("unknown_error")
            );
        }
        Ok(response)
    }
}

/// Longest path a Unix socket address can hold, without its terminator.
const MAX_SOCKET_PATH_BYTES: usize = 107;

/// Connect to a host-control socket wherever its directory is.
///
/// A socket address holds at most 107 path bytes, and a preview runtime's
/// control socket lies deeper than that. Linux resolves a path through an
/// open directory handle, which keeps the address short for any depth.
async fn connect(socket: &Path) -> std::io::Result<UnixStream> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    if socket.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH_BYTES || !cfg!(target_os = "linux") {
        return UnixStream::connect(socket).await;
    }
    let (Some(directory), Some(name)) = (socket.parent(), socket.file_name()) else {
        return UnixStream::connect(socket).await;
    };
    let directory = std::fs::File::open(directory)?;
    let through_handle = Path::new("/proc/self/fd")
        .join(directory.as_raw_fd().to_string())
        .join(name);
    UnixStream::connect(through_handle).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn message_request_requests_a_chat_copy() {
        use std::path::PathBuf;
        use tokio::{
            io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
            net::UnixListener,
        };

        let path = PathBuf::from("/tmp").join(format!("kurbis-message-{}.sock", Uuid::new_v4()));
        let listener = UnixListener::bind(&path).unwrap();
        let player_id = Uuid::new_v4();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read_half)
                .read_line(&mut line)
                .await
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["action"], "message");
            assert_eq!(request["player_id"], player_id.to_string());
            assert_eq!(request["message"], "Queued for build…");
            assert_eq!(request["chat"], true);
            write_half.write_all(b"{\"ok\":true}\n").await.unwrap();
        });

        PumpkinControl::new("test-token".into())
            .message(&path, player_id, "Queued for build…")
            .await
            .unwrap();
        server.await.unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn host_control_reports_verified_java_and_bedrock_identities_per_connection() {
        let connection_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        let java_uuid = Uuid::new_v4();
        let java: WaitingPlayer = serde_json::from_value(json!({
            "connection_id": connection_id,
            "id": player_id,
            "name": "Java Player",
            "edition": "java",
            "server_address": "alpha.mc.example.test",
            "ready_for_transfer": true,
            "verified_identity": {"kind":"java","uuid":java_uuid}
        }))
        .unwrap();
        assert_eq!(java.connection_id, connection_id);
        assert_eq!(
            java.verified_identity,
            Some(VerifiedMinecraftIdentity::Java { uuid: java_uuid })
        );

        let bedrock: WaitingPlayer = serde_json::from_value(json!({
            "connection_id": Uuid::new_v4(),
            "id": Uuid::new_v4(),
            "name": "Bedrock Player",
            "edition": "bedrock",
            "server_address": "alpha.mc.example.test",
            "ready_for_transfer": true,
            "verified_identity": {"kind":"bedrock","xuid":"2533274790395904"}
        }))
        .unwrap();
        assert_eq!(
            bedrock.verified_identity,
            Some(VerifiedMinecraftIdentity::Bedrock {
                xuid: "2533274790395904".to_owned()
            })
        );
    }

    #[test]
    fn host_stats_response_preserves_unavailable_resource_fields() {
        let stats: HostStats = serde_json::from_value(json!({
            "cpu_percent": null,
            "cpu_limit_millis": 1000,
            "memory_used_bytes": 12,
            "memory_limit_bytes": 2048,
            "disk_used_bytes": null,
            "disk_limit_bytes": null,
            "tps": 20.0
        }))
        .unwrap();
        assert_eq!(stats.cpu_percent, None);
        assert_eq!(stats.cpu_limit_millis, Some(1000));
        assert_eq!(stats.tps, Some(20.0));
    }

    #[test]
    fn offline_runtime_report_has_no_verified_identity() {
        let player: WaitingPlayer = serde_json::from_value(json!({
            "connection_id": Uuid::new_v4(),
            "id": Uuid::new_v4(),
            "name": "Offline Player",
            "edition": "java",
            "server_address": "alpha.mc.example.test",
            "ready_for_transfer": true,
            "verified_identity": null
        }))
        .unwrap();
        assert_eq!(player.verified_identity, None);
    }
}
