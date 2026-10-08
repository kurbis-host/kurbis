use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_REQUEST_BYTES: u64 = 64 * 1024;
pub const OPERATOR_AUTHORIZATION_TTL_MS: u128 = 5_000;
pub const MAX_PLUGIN_DATA_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_QUEUE_JOBS: usize = 64;
pub const MAX_QUEUE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_EVENT_LOG_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_EVENT_MESSAGE_BYTES: usize = 8 * 1024;
pub const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_WORKSPACE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_MCP_ARGUMENT_BYTES: usize = 16 * 1024;
pub const MAX_MCP_REQUEST_BYTES: u64 = 32 * 1024;
pub const MAX_MCP_RESULT_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobAction {
    Code,
    Revert,
    Reload,
    Ask,
    Login,
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
    #[serde(default)]
    pub operator_uuid: Option<Uuid>,
    pub created_unix_ms: u128,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct OperatorAuthorizationSnapshot {
    pub updated_unix_ms: u128,
    pub operator_uuids: Vec<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpJobLease {
    pub job_id: Uuid,
    pub operator_uuid: Uuid,
    pub created_unix_ms: u128,
    pub expires_unix_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpActionRequest {
    pub id: Uuid,
    pub job_id: Uuid,
    pub tool: String,
    pub arguments: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpActionResult {
    pub id: Uuid,
    pub job_id: Uuid,
    pub tool: String,
    pub operator_uuid: Uuid,
    pub arguments: serde_json::Value,
    pub success: bool,
    pub output: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrivateAuthStatus {
    CodeAvailable,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PrivateAuthMessage {
    pub job_id: Uuid,
    pub player_uuid: Uuid,
    pub player_name: String,
    pub status: PrivateAuthStatus,
    pub verification_url: Option<String>,
    pub user_code: Option<String>,
    pub expires_unix_ms: u128,
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
    anyhow::ensure!(name != "kurbis-ai", "kurbis-ai is reserved");
    Ok(())
}

pub fn validate_request(request: &JobRequest) -> anyhow::Result<()> {
    if request.action == JobAction::Login {
        anyhow::ensure!(
            request.plugin == "kurbis-ai",
            "login requests must target kurbis-ai"
        );
    } else {
        validate_plugin_name(&request.plugin)?;
    }
    anyhow::ensure!(
        !request.player.trim().is_empty(),
        "player must not be empty"
    );
    match request.action {
        JobAction::Code | JobAction::Ask => {
            anyhow::ensure!(
                !request.prompt.trim().is_empty(),
                "prompt must not be empty"
            );
            anyhow::ensure!(
                request.prompt.len() <= MAX_PROMPT_BYTES,
                "prompt exceeds {MAX_PROMPT_BYTES} bytes"
            );
        }
        JobAction::Revert | JobAction::Reload | JobAction::Login => {
            anyhow::ensure!(
                request.prompt.is_empty(),
                "non-prompt requests must not include a prompt"
            );
        }
    }
    anyhow::ensure!(
        request.operator_uuid.is_some(),
        "operator identity is required for every action"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_pages_do_not_expose_partial_live_records_or_lose_long_unicode_output() {
        let dir = std::env::temp_dir().join(format!("kurbis-ai-events-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        assert!(event_page(&path, 0, 2).unwrap().is_empty());
        let message = "🎃 full output ".repeat(1000);
        let records: String = (1..=3)
            .map(|sequence| {
                serde_json::to_string(&ProgressEvent {
                    sequence,
                    kind: "item.completed".into(),
                    message: message.clone(),
                    raw: serde_json::json!({"sequence":sequence}),
                    unix_ms: 0,
                })
                .unwrap()
                    + "\n"
            })
            .collect();
        std::fs::write(&path, records + "{\"sequence\":4").unwrap();
        let first = event_page(&path, 0, 2).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].message, message);
        let second = event_page(&path, 1, 2).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].sequence, 3);
        assert!(event_page(&path, usize::MAX, 2).unwrap().is_empty());
        assert!(event_page(&path, 0, 0).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn truncates_unicode_only_at_codepoint_boundaries() {
        assert_eq!(truncate_utf8("🎃abc", 0), "");
        assert_eq!(truncate_utf8("🎃abc", 1), "");
        assert_eq!(truncate_utf8("🎃abc", 4), "🎃");
        assert_eq!(truncate_utf8("🎃abc", 6), "🎃ab");
        assert_eq!(truncate_utf8("🎃abc", 100), "🎃abc");
    }

    #[test]
    fn settings_reject_invalid_values_and_restore_defaults() {
        let mut settings = Settings::default();
        assert!(settings.validate().is_ok());
        for value in ["model with spaces", "$(touch /tmp/x)", "\n", "a; b"] {
            settings.model = value.into();
            assert!(settings.validate().is_err());
        }
        settings.model = "provider/model-v2".into();
        assert!(settings.validate().is_ok());
        settings.reasoning = "unsupported".into();
        assert!(settings.validate().is_err());
        settings = Settings::default();
        for timeout in [0, 29, 3601, u64::MAX] {
            settings.timeout_seconds = timeout;
            assert!(settings.validate().is_err());
        }
        for timeout in [30, 900, 1800, 3600] {
            settings.timeout_seconds = timeout;
            assert!(settings.validate().is_ok());
        }
        let restored: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(restored.timeout_seconds, 900);
        assert!(restored.chat_progress);
    }

    #[test]
    fn server_control_and_login_require_a_bound_operator() {
        let mut request = JobRequest {
            id: Uuid::new_v4(),
            session_id: None,
            action: JobAction::Ask,
            plugin: "server-control".into(),
            prompt: "spawn a pig".into(),
            player: "Operator".into(),
            operator_uuid: None,
            created_unix_ms: 0,
        };
        assert!(validate_request(&request).is_err());
        request.operator_uuid = Some(Uuid::new_v4());
        assert!(validate_request(&request).is_ok());
        request.action = JobAction::Login;
        request.plugin = "kurbis-ai".into();
        request.prompt.clear();
        assert!(validate_request(&request).is_ok());
        request.operator_uuid = None;
        assert!(validate_request(&request).is_err());
    }

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
            "kurbis-ai",
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
    pub operator_uuid: Uuid,
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
/// Truncate a UTF-8 string without ever splitting a code point.
pub fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = 0;
    for (index, character) in value.char_indices() {
        let next = index + character.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    value[..end].to_owned()
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

/// Read only complete JSONL records, including while a writer appends the next event.
pub fn event_page(
    path: &std::path::Path,
    page: usize,
    size: usize,
) -> anyhow::Result<Vec<ProgressEvent>> {
    use std::io::{BufRead, BufReader};
    if size == 0 {
        return Ok(vec![]);
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut reader = BufReader::new(file);
    let mut events = vec![];
    let mut line = String::new();
    let start = page.saturating_mul(size);
    let mut index = 0;
    while reader.read_line(&mut line)? > 0 {
        if !line.ends_with('\n') {
            break;
        }
        if index >= start {
            events.push(serde_json::from_str(&line)?);
            if events.len() >= size {
                break;
            }
        }
        index += 1;
        line.clear();
    }
    Ok(events)
}
