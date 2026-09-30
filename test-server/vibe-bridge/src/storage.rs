use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};
use uuid::Uuid;
use vibe_protocol::{
    JobAction, JobRequest, JobResult, JobState, ProgressEvent, Sessions, Settings, now_ms,
    read_json, validate_request, write_json,
};

pub fn root() -> &'static Path {
    Path::new("data")
}
pub fn path(file: &str) -> PathBuf {
    root().join(file)
}
pub fn initialize() -> anyhow::Result<()> {
    for dir in ["requests", "results", "cancel", "events", "progress"] {
        fs::create_dir_all(path(dir))?;
    }
    if !path("settings.json").exists() {
        write_json(&path("settings.json"), &Settings::default())?;
    }
    Ok(())
}
pub fn enqueue(
    action: JobAction,
    plugin: String,
    prompt: String,
    player: String,
    session_id: Option<Uuid>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let request = JobRequest {
        id,
        session_id,
        action,
        plugin,
        prompt,
        player,
        created_unix_ms: now_ms(),
    };
    validate_request(&request)?;
    write_json(
        &path("results").join(format!("{id}.json")),
        &JobResult {
            id,
            plugin: request.plugin.clone(),
            state: JobState::Queued,
            message: "Waiting for the Codex worker".into(),
            updated_unix_ms: now_ms(),
        },
    )?;
    write_json(&path("requests").join(format!("{id}.json")), &request)?;
    Ok(id)
}
pub fn continue_session(id: Uuid, prompt: String, player: String) -> anyhow::Result<Uuid> {
    let sessions: Sessions = read_json(&path("sessions.json"))?;
    let session = sessions
        .sessions
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| anyhow::anyhow!("Session not found"))?;
    anyhow::ensure!(
        session.thread_id.is_some(),
        "Session is not ready to continue"
    );
    enqueue(session.action, session.plugin, prompt, player, Some(id))
}
pub fn cancel(id: Uuid) -> anyhow::Result<()> {
    write_json(
        &path("cancel").join(format!("{id}.json")),
        &serde_json::json!({"cancel":true}),
    )
}
pub fn jobs() -> anyhow::Result<Vec<JobResult>> {
    let mut jobs = vec![];
    for entry in fs::read_dir(path("results"))? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|e| e == "json") {
            jobs.push(serde_json::from_slice::<JobResult>(&fs::read(
                entry.path(),
            )?)?);
        }
    }
    jobs.sort_by_key(|j| std::cmp::Reverse(j.updated_unix_ms));
    Ok(jobs)
}
pub fn event_page(id: Uuid, page: usize, size: usize) -> anyhow::Result<Vec<ProgressEvent>> {
    let file = match fs::File::open(path("events").join(format!("{id}.jsonl"))) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    BufReader::new(file)
        .lines()
        .skip(page.saturating_mul(size))
        .take(size)
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}
