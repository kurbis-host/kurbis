use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_LOG_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobAction {
    Code,
    Revert,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JobRequest {
    pub id: Uuid,
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
    Ok(())
}

pub fn validate_request(request: &JobRequest) -> anyhow::Result<()> {
    validate_plugin_name(&request.plugin)?;
    anyhow::ensure!(
        !request.player.trim().is_empty(),
        "player must not be empty"
    );
    match request.action {
        JobAction::Code => {
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
