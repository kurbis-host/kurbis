use kurbis_ai_protocol::{
    MAX_EVENT_LOG_BYTES, MAX_EVENT_MESSAGE_BYTES, Progress, ProgressEvent, now_ms, truncate_utf8,
    write_json,
};
use serde_json::{Value, json};
use std::{io::Write, path::PathBuf};
use uuid::Uuid;

pub struct Recorder {
    job: Uuid,
    log: std::fs::File,
    snapshot: PathBuf,
    progress: Progress,
    bytes_written: u64,
    quota_notified: bool,
}
impl Recorder {
    pub fn new(data: &std::path::Path, job: Uuid) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data.join("events"))?;
        std::fs::create_dir_all(data.join("progress"))?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(data.join("events").join(format!("{job}.jsonl")))?;
        let snapshot = data.join("progress").join(format!("{job}.json"));
        let progress = kurbis_ai_protocol::read_json(&snapshot)?;
        let bytes_written = log.metadata()?.len();
        Ok(Self {
            job,
            log,
            snapshot,
            progress,
            bytes_written,
            quota_notified: bytes_written >= MAX_EVENT_LOG_BYTES,
        })
    }
    pub fn line(&mut self, line: &str, stderr: bool) -> anyhow::Result<()> {
        let bounded_line = truncate_utf8(line, MAX_EVENT_MESSAGE_BYTES);
        let raw = if stderr {
            json!({"type":"stderr","text":bounded_line})
        } else {
            serde_json::from_str(&bounded_line)
                .unwrap_or_else(|_| json!({"type":"stdout","text":bounded_line}))
        };
        if raw["type"] == "thread.started" {
            self.progress.thread_id = raw["thread_id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok());
        }
        let kind = raw["type"].as_str().unwrap_or("unknown").to_owned();
        let message = describe(&raw);
        self.record(kind, message, raw)
    }
    pub fn prompt(&mut self, player: &str, prompt: &str) -> anyhow::Result<()> {
        self.record(
            "user.prompt".into(),
            truncate_utf8(prompt, MAX_EVENT_MESSAGE_BYTES),
            json!({"player":player, "prompt":truncate_utf8(prompt, MAX_EVENT_MESSAGE_BYTES)}),
        )
    }
    pub fn step(&mut self, message: &str) -> anyhow::Result<()> {
        let message = truncate_utf8(message, MAX_EVENT_MESSAGE_BYTES);
        self.record(
            "controller".into(),
            message.clone(),
            json!({"text":message}),
        )
    }
    fn record(&mut self, kind: String, message: String, raw: Value) -> anyhow::Result<()> {
        if self.quota_notified {
            return Ok(());
        }
        let message = truncate_utf8(&message, MAX_EVENT_MESSAGE_BYTES);
        let mut event = ProgressEvent {
            sequence: self.progress.total_events.saturating_add(1),
            kind,
            message,
            raw,
            unix_ms: now_ms(),
        };
        let mut encoded = serde_json::to_vec(&event)?;
        if self
            .bytes_written
            .saturating_add(encoded.len() as u64)
            .saturating_add(1)
            > MAX_EVENT_LOG_BYTES
        {
            event.kind = "controller".into();
            event.message = "Job output quota reached; further output was discarded.".into();
            event.raw = Value::Null;
            encoded = serde_json::to_vec(&event)?;
            if self
                .bytes_written
                .saturating_add(encoded.len() as u64)
                .saturating_add(1)
                <= MAX_EVENT_LOG_BYTES
            {
                self.log.write_all(&encoded)?;
                self.log.write_all(b"\n")?;
                self.log.flush()?;
                self.bytes_written = self.bytes_written.saturating_add(encoded.len() as u64 + 1);
                self.progress.total_events = event.sequence;
                self.progress.events.push(event.clone());
                if self.progress.events.len() > 100 {
                    self.progress.events.remove(0);
                }
                write_json(&self.snapshot, &self.progress)?;
            }
            self.quota_notified = true;
            return Ok(());
        }
        self.log.write_all(&encoded)?;
        self.log.write_all(b"\n")?;
        self.log.flush()?;
        self.bytes_written = self.bytes_written.saturating_add(encoded.len() as u64 + 1);
        self.progress.total_events = event.sequence;
        println!(
            "[Kurbis AI {} #{}] {}: {}",
            self.job, event.sequence, event.kind, event.message
        );
        let mut preview = event;
        preview.message = truncate_utf8(&preview.message, 2000);
        preview.raw = Value::Null;
        self.progress.events.push(preview);
        if self.progress.events.len() > 100 {
            self.progress.events.remove(0);
        }
        write_json(&self.snapshot, &self.progress)?;
        Ok(())
    }
}
fn describe(event: &Value) -> String {
    let item = event.get("item").unwrap_or(event);
    let mut parts = vec![];
    for key in [
        "type",
        "command",
        "text",
        "aggregated_output",
        "status",
        "message",
        "error",
        "changes",
        "usage",
    ] {
        if let Some(value) = item.get(key) {
            let text = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            if !text.is_empty() {
                parts.push(text);
            }
        }
    }
    if parts.is_empty() {
        event.to_string()
    } else {
        parts.join("\n")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retains_every_raw_event_while_bounding_the_preview() {
        let temp = tempfile::tempdir().unwrap();
        let job = Uuid::new_v4();
        let mut recorder = Recorder::new(temp.path(), job).unwrap();
        recorder
            .line(
                r#"{"type":"thread.started","thread_id":"11111111-1111-4111-8111-111111111111"}"#,
                false,
            )
            .unwrap();
        for n in 0..120 {
            recorder.line(&format!(r#"{{"type":"item.completed","item":{{"type":"command_execution","command":"echo {n}","aggregated_output":"output {n}"}}}}"#), false).unwrap();
        }
        recorder.line("diagnostic", true).unwrap();
        let content =
            std::fs::read_to_string(temp.path().join("events").join(format!("{job}.jsonl")))
                .unwrap();
        assert_eq!(content.lines().count(), 122);
        assert!(content.contains("output 0"));
        let preview: Progress = kurbis_ai_protocol::read_json(&recorder.snapshot).unwrap();
        assert_eq!(preview.events.len(), 100);
        assert_eq!(preview.total_events, 122);
        assert!(preview.thread_id.is_some());
        assert_eq!(preview.events.last().unwrap().sequence, 122);
    }
}
