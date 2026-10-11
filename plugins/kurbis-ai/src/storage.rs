use kurbis_ai_protocol::{
    JobAction, JobRequest, JobResult, JobState, MAX_PLUGIN_DATA_BYTES, MAX_QUEUE_BYTES,
    MAX_QUEUE_JOBS, OperatorAuthorizationSnapshot, ProgressEvent, Sessions, Settings, now_ms,
    read_json, validate_request, write_json,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub fn root() -> &'static Path {
    Path::new("data")
}
pub fn path(file: &str) -> PathBuf {
    root().join(file)
}
pub fn initialize() -> anyhow::Result<()> {
    for dir in [
        "requests",
        "results",
        "cancel",
        "events",
        "progress",
        "mcp/jobs",
        "mcp/requests",
        "mcp/results",
        "mcp/audit",
        "private/auth",
        "authorization",
    ] {
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
    operator_uuid: Option<Uuid>,
    session_id: Option<Uuid>,
) -> anyhow::Result<Uuid> {
    enforce_queue_quota()?;
    let id = Uuid::new_v4();
    let request = JobRequest {
        id,
        session_id,
        action,
        plugin,
        prompt,
        player,
        operator_uuid,
        created_unix_ms: now_ms(),
    };
    validate_request(&request)?;
    write_json(
        &path("results").join(format!("{id}.json")),
        &JobResult {
            id,
            plugin: request.plugin.clone(),
            state: JobState::Queued,
            message: "Waiting for the workshop worker".into(),
            updated_unix_ms: now_ms(),
        },
    )?;
    write_json(&path("requests").join(format!("{id}.json")), &request)?;
    Ok(id)
}
pub fn continue_session(
    id: Uuid,
    prompt: String,
    player: String,
    operator_uuid: Option<Uuid>,
) -> anyhow::Result<Uuid> {
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
    let operator_uuid = operator_uuid.ok_or_else(|| anyhow::anyhow!("Operator access required"))?;
    anyhow::ensure!(
        operator_uuid == session.operator_uuid,
        "Only the operator who started this session may continue it"
    );
    enqueue(
        session.action,
        session.plugin,
        prompt,
        player,
        Some(operator_uuid),
        Some(id),
    )
}
fn enforce_queue_quota() -> anyhow::Result<()> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    for directory in [path("requests"), path("processing")] {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() || entry.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            count = count.saturating_add(1);
            bytes = bytes.saturating_add(entry.metadata()?.len());
        }
    }
    anyhow::ensure!(
        count < MAX_QUEUE_JOBS,
        "Kurbis AI queue is full; wait for a job to finish"
    );
    anyhow::ensure!(
        bytes < MAX_QUEUE_BYTES,
        "Kurbis AI queue storage is full; wait for a job to finish"
    );
    let mut disk_bytes = 0u64;
    let mut directories = vec![path("").to_path_buf()];
    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "Kurbis AI data cannot contain symlinks"
            );
            if metadata.file_type().is_dir() {
                directories.push(entry.path());
            } else if metadata.file_type().is_file() {
                disk_bytes = disk_bytes.saturating_add(metadata.len());
                anyhow::ensure!(
                    disk_bytes <= MAX_PLUGIN_DATA_BYTES,
                    "Kurbis AI data has reached its {} byte quota",
                    MAX_PLUGIN_DATA_BYTES
                );
            }
        }
    }
    Ok(())
}

pub fn reauthorize_pending(server: &pumpkin_plugin_api::Server) {
    let operator_uuids: Vec<Uuid> = server
        .get_all_players()
        .into_iter()
        .filter(|player| player.has_permission(crate::PERMISSION))
        .filter_map(|player| {
            Uuid::parse_str(&pumpkin_plugin_api::uuid::to_string(player.get_id())).ok()
        })
        .collect();
    let _ = write_json(
        &path("authorization/operators.json"),
        &OperatorAuthorizationSnapshot {
            updated_unix_ms: now_ms(),
            operator_uuids: operator_uuids.clone(),
        },
    );
    let directory = path("requests");
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten().take(MAX_QUEUE_JOBS) {
        let file_path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&file_path) else {
            continue;
        };
        if !metadata.file_type().is_file()
            || metadata.len() > kurbis_ai_protocol::MAX_REQUEST_BYTES
            || file_path.extension().is_none_or(|e| e != "json")
        {
            continue;
        }
        let Ok(bytes) = fs::read(&file_path) else {
            continue;
        };
        let Ok(request) = serde_json::from_slice::<JobRequest>(&bytes) else {
            let _ = fs::remove_file(&file_path);
            continue;
        };
        if request
            .operator_uuid
            .is_some_and(|expected| operator_uuids.contains(&expected))
        {
            continue;
        }
        let result = JobResult {
            id: request.id,
            plugin: request.plugin,
            state: JobState::Cancelled,
            message: "Operator permission was revoked before the job started.".into(),
            updated_unix_ms: now_ms(),
        };
        let _ = write_json(
            &path("results").join(format!("{}.json", request.id)),
            &result,
        );
        let _ = fs::remove_file(&file_path);
    }
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
    kurbis_ai_protocol::event_page(&path("events").join(format!("{id}.jsonl")), page, size)
}
