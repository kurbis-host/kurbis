use serde_json::{Value, json};
use std::{io::Write, path::PathBuf};
use uuid::Uuid;
use vibe_protocol::{Progress, ProgressEvent, now_ms, write_json};

pub struct Recorder {
    job: Uuid,
    log: std::fs::File,
    snapshot: PathBuf,
    progress: Progress,
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
        let progress = vibe_protocol::read_json(&snapshot)?;
        Ok(Self {
            job,
            log,
            snapshot,
            progress,
        })
    }
    pub fn line(&mut self, line: &str, stderr: bool) -> anyhow::Result<()> {
        let raw = if stderr {
            json!({"type":"stderr","text":line})
        } else {
            serde_json::from_str(line).unwrap_or_else(|_| json!({"type":"stdout","text":line}))
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
    pub fn step(&mut self, message: &str) -> anyhow::Result<()> {
        self.record("controller".into(), message.into(), json!({"text":message}))
    }
    fn record(&mut self, kind: String, message: String, raw: Value) -> anyhow::Result<()> {
        self.progress.total_events += 1;
        let event = ProgressEvent {
            sequence: self.progress.total_events,
            kind,
            message,
            raw,
            unix_ms: now_ms(),
        };
        // The complete event is retained on disk and printed, including command output.
        writeln!(self.log, "{}", serde_json::to_string(&event)?)?;
        self.log.flush()?;
        println!(
            "[vibe {} #{}] {}: {}",
            self.job, event.sequence, event.kind, event.message
        );
        let mut preview = event;
        preview.message = preview.message.chars().take(2000).collect();
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
        let preview: Progress = vibe_protocol::read_json(&recorder.snapshot).unwrap();
        assert_eq!(preview.events.len(), 100);
        assert_eq!(preview.total_events, 122);
        assert!(preview.thread_id.is_some());
        assert_eq!(preview.events.last().unwrap().sequence, 122);
    }
}
