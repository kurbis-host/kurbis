use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_PROMPT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobAction {
    Code,
    Revert,
    Chat,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobRequest {
    pub id: Uuid,
    #[serde(default)]
    pub session_id: Option<Uuid>,
    pub action: JobAction,
    pub plugin: String,
    #[serde(default)]
    pub prompt: String,
    pub player: String,
    pub created_unix_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Validating,
    Installed,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobResult {
    pub id: Uuid,
    pub plugin: String,
    pub state: JobState,
    pub message: String,
    pub updated_unix_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct PluginIndex {
    pub plugins: Vec<PluginIndexEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginIndexEntry {
    pub name: String,
    pub revision: String,
    pub artifact_bytes: u64,
}

pub fn validate_plugin_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1..=64).contains(&name.len()),
        "plugin name must be 1..=64 bytes"
    );
    anyhow::ensure!(
        name.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "plugin name may contain only lowercase ASCII letters, digits, and '-'"
    );
    anyhow::ensure!(
        name.as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && name
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric),
        "plugin name must start and end with an ASCII letter or digit"
    );
    anyhow::ensure!(
        !name.contains("--"),
        "plugin name must not contain consecutive '-'"
    );
    anyhow::ensure!(name != "vibe-bridge", "vibe-bridge is reserved");
    Ok(())
}

pub fn validate_request(request: &JobRequest) -> anyhow::Result<()> {
    validate_plugin_name(&request.plugin)?;
    anyhow::ensure!(
        !request.player.trim().is_empty(),
        "player must not be empty"
    );
    match request.action {
        JobAction::Code | JobAction::Chat => {
            anyhow::ensure!(
                !request.prompt.trim().is_empty(),
                "prompt must not be empty"
            );
            anyhow::ensure!(
                request.prompt.len() <= MAX_PROMPT_BYTES,
                "prompt exceeds {MAX_PROMPT_BYTES} bytes"
            );
        }
        JobAction::Revert => {
            anyhow::ensure!(
                request.prompt.is_empty(),
                "revert requests must not include a prompt"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_plugin_slugs() {
        for valid in ["hello", "hello-world", "a1", "plugin-42"] {
            assert!(validate_plugin_name(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "../x",
            "Hello",
            "-hello",
            "hello-",
            "hello--world",
            "a_b",
            "a/b",
        ] {
            assert!(validate_plugin_name(invalid).is_err(), "{invalid}");
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub model: String,
    pub reasoning: String,
    pub timeout_seconds: u64,
    pub chat_progress: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            model: String::new(),
            reasoning: "medium".into(),
            timeout_seconds: 900,
            chat_progress: true,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.model.len() <= 128
                && self
                    .model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./:".contains(c)),
            "invalid model identifier"
        );
        anyhow::ensure!(
            ["minimal", "low", "medium", "high", "xhigh"].contains(&self.reasoning.as_str()),
            "unsupported reasoning effort"
        );
        anyhow::ensure!(
            (30..=3600).contains(&self.timeout_seconds),
            "timeout must be 30..=3600 seconds"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProgressEvent {
    pub sequence: u64,
    pub kind: String,
    pub message: String,
    pub raw: serde_json::Value,
    pub unix_ms: u128,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Progress {
    pub thread_id: Option<Uuid>,
    pub total_events: u64,
    pub events: Vec<ProgressEvent>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Turn {
    pub job_id: Uuid,
    pub prompt: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Session {
    pub id: Uuid,
    pub thread_id: Option<Uuid>,
    pub plugin: String,
    pub player: String,
    pub action: JobAction,
    pub title: String,
    pub turns: Vec<Turn>,
    pub updated_unix_ms: u128,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Sessions {
    pub sessions: Vec<Session>,
}
pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
pub fn read_json<T: serde::de::DeserializeOwned + Default>(
    path: &std::path::Path,
) -> anyhow::Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error.into()),
    }
}
pub fn write_json(path: &std::path::Path, value: &impl Serialize) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing parent"))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    std::fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}
