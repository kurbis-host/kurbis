use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};
use uuid::Uuid;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerEdition {
    Java,
    Bedrock,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WaitingPlayer {
    pub id: Uuid,
    pub name: String,
    pub edition: PlayerEdition,
    pub server_address: String,
}

#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    players: Option<usize>,
    waiting_players: Option<Vec<WaitingPlayer>>,
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

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub async fn status(&self, socket: &Path) -> Result<usize> {
        let response = self.request(socket, json!({"action":"status"})).await?;
        response.players.context("status response omitted players")
    }

    pub async fn players(&self, socket: &Path) -> Result<Vec<WaitingPlayer>> {
        let response = self.request(socket, json!({"action":"players"})).await?;
        response
            .waiting_players
            .context("players response omitted waiting_players")
    }

    pub async fn message(&self, socket: &Path, player_id: Uuid, message: &str) -> Result<()> {
        self.request(
            socket,
            json!({"action":"message", "player_id":player_id, "message":message}),
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

    pub async fn save(&self, socket: &Path) -> Result<()> {
        self.request(socket, json!({"action":"save"})).await?;
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

        let stream = timeout(IO_TIMEOUT, UnixStream::connect(socket))
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
