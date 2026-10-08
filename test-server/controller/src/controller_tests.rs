use super::*;

#[test]
fn artifact_name_uses_rust_crate_normalization() {
    let path = artifact_path(Path::new("/tmp/foo"), "hello-world");
    assert!(path.ends_with("target/wasm32-wasip2/release/hello_world.wasm"));
}

#[test]
fn codex_prompt_keeps_request_as_data() {
    let prompt = codex_prompt("demo", "Player", "add /hello");
    assert!(prompt.contains("PLAYER REQUEST:\nadd /hello"));
}

#[tokio::test]
async fn plugin_index_only_lists_installed_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = RuntimePaths::new(temp.path());
    paths.create_dirs().await.expect("runtime dirs");

    let plugin_dir = paths.plugin_workspace("demo");
    fs::create_dir_all(&plugin_dir).await.expect("plugin dir");
    fs::write(plugin_dir.join("README.md"), "demo")
        .await
        .expect("plugin file");
    git_status(&plugin_dir, &["init"]).await.expect("git init");
    git_status(&plugin_dir, &["add", "-A"])
        .await
        .expect("git add");
    git_status(
        &plugin_dir,
        &[
            "-c",
            "user.name=Kurbis AI Test",
            "-c",
            "user.email=kurbis-ai-test@localhost",
            "commit",
            "-m",
            "test fixture",
        ],
    )
    .await
    .expect("git commit");

    let controller = Controller::new(paths.clone(), "true".into(), Duration::from_secs(1));
    controller
        .refresh_plugin_index()
        .await
        .expect("refresh empty index");
    let index: PluginIndex =
        serde_json::from_slice(&fs::read(paths.plugin_index()).await.expect("empty index"))
            .expect("parse empty index");
    assert!(index.plugins.is_empty());

    fs::write(paths.installed_plugin("demo"), b"wasm")
        .await
        .expect("deployed artifact");
    controller
        .refresh_plugin_index()
        .await
        .expect("refresh deployed index");
    let index: PluginIndex = serde_json::from_slice(
        &fs::read(paths.plugin_index())
            .await
            .expect("deployed index"),
    )
    .expect("parse deployed index");
    assert_eq!(index.plugins.len(), 1);
    assert_eq!(index.plugins[0].name, "demo");
    assert_eq!(index.plugins[0].artifact_bytes, 4);
}

#[tokio::test]
async fn reset_plugin_workspace_discards_failed_job_changes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let plugin_dir = temp.path().join("plugin");
    fs::create_dir_all(&plugin_dir).await.expect("plugin dir");
    fs::write(plugin_dir.join("tracked.txt"), "known good")
        .await
        .expect("tracked file");
    git_status(&plugin_dir, &["init"]).await.expect("git init");
    git_status(&plugin_dir, &["add", "-A"])
        .await
        .expect("git add");
    git_status(
        &plugin_dir,
        &[
            "-c",
            "user.name=Kurbis AI Test",
            "-c",
            "user.email=kurbis-ai-test@localhost",
            "commit",
            "-m",
            "known good",
        ],
    )
    .await
    .expect("git commit");

    fs::write(plugin_dir.join("tracked.txt"), "failed change")
        .await
        .expect("modify tracked file");
    fs::write(plugin_dir.join("untracked.txt"), "partial output")
        .await
        .expect("untracked file");

    reset_plugin_workspace(&plugin_dir)
        .await
        .expect("reset workspace");

    assert_eq!(
        fs::read_to_string(plugin_dir.join("tracked.txt"))
            .await
            .expect("tracked content"),
        "known good"
    );
    assert!(
        !fs::try_exists(plugin_dir.join("untracked.txt"))
            .await
            .expect("untracked existence")
    );
    assert!(
        git_output(&plugin_dir, &["status", "--porcelain"])
            .await
            .expect("git status")
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn installation_and_revert_stage_revisions_and_reject_invalid_updates() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let paths = RuntimePaths::new(temp.path().join("runtime"));
    paths.create_dirs().await.expect("runtime");
    let worker = temp.path().join("worker");
    fs::write(
        &worker,
        r#"#!/bin/sh
cd "$2" || exit 1
cat > job.txt
mkdir -p target/wasm32-wasip2/release
printf '\000asm\015\000\001\000' > target/wasm32-wasip2/release/demo.wasm
"#,
    )
    .await
    .expect("worker");
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755))
        .expect("executable worker");
    let controller = Controller::new(
        paths.clone(),
        worker.to_string_lossy().into_owned(),
        Duration::from_secs(5),
    );

    async fn submit(
        controller: &Controller,
        paths: &RuntimePaths,
        action: JobAction,
        prompt: &str,
    ) -> JobResult {
        let request = JobRequest {
            id: Uuid::new_v4(),
            session_id: None,
            plugin: "demo".into(),
            action,
            prompt: prompt.into(),
            player: "Operator".into(),
            operator_uuid: Some(Uuid::new_v4()),
            created_unix_ms: now_ms(),
        };
        write_json(
            &paths.operator_authorizations(),
            &OperatorAuthorizationSnapshot {
                updated_unix_ms: now_ms(),
                operator_uuids: vec![request.operator_uuid.unwrap()],
            },
        )
        .expect("authorization snapshot");
        write_json(
            &paths.requests().join(format!("{}.json", request.id)),
            &request,
        )
        .expect("queue job");
        let _ = controller.run_once().await;
        serde_json::from_slice(
            &fs::read(paths.results().join(format!("{}.json", request.id)))
                .await
                .expect("result"),
        )
        .expect("parse result")
    }

    let first = submit(&controller, &paths, JobAction::Code, "first version").await;
    assert_eq!(first.state, JobState::Installed);
    assert!(first.message.contains("reload pending"));
    let plugin = paths.plugin_workspace("demo");
    let first_revision = git_output(&plugin, &["rev-parse", "HEAD"])
        .await
        .expect("revision");
    let first_bytes = fs::read(paths.installed_plugin("demo"))
        .await
        .expect("installed");
    let second = submit(&controller, &paths, JobAction::Code, "second version").await;
    assert_eq!(second.state, JobState::Installed);
    assert_ne!(
        git_output(&plugin, &["rev-parse", "HEAD"])
            .await
            .expect("revision"),
        first_revision
    );
    let reverted = submit(&controller, &paths, JobAction::Revert, "").await;
    assert_eq!(reverted.state, JobState::Installed);
    assert!(reverted.message.contains("reload pending"));
    assert_eq!(
        git_output(&plugin, &["rev-parse", "HEAD"])
            .await
            .expect("revision"),
        first_revision
    );
    assert_eq!(
        fs::read(paths.installed_plugin("demo"))
            .await
            .expect("restored"),
        first_bytes
    );

    fs::write(
        &worker,
        r#"#!/bin/sh
cd "$2" || exit 1
cat > job.txt
printf broken > target/wasm32-wasip2/release/demo.wasm
"#,
    )
    .await
    .expect("invalid worker");
    let failed = submit(&controller, &paths, JobAction::Code, "invalid version").await;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(
        fs::read(paths.installed_plugin("demo"))
            .await
            .expect("preserved"),
        first_bytes
    );
    assert_eq!(
        git_output(&plugin, &["rev-parse", "HEAD"])
            .await
            .expect("revision"),
        first_revision
    );
    assert!(
        git_output(&plugin, &["status", "--porcelain"])
            .await
            .expect("clean")
            .is_empty()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn sessions_resume_saved_threads_apply_settings_and_reject_invalid_requests() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::new(temp.path().join("runtime"));
    paths.create_dirs().await.unwrap();
    let worker = temp.path().join("worker");
    fs::write(&worker, r#"#!/usr/bin/env python3
import sys,json,pathlib
args=sys.argv[1:]
prompt=sys.stdin.read()
with (pathlib.Path(__file__).parent / 'calls.jsonl').open('a') as f:
 f.write(json.dumps({'args':args,'prompt':prompt})+'\n')
thread=args[args.index('resume')+1] if 'resume' in args else '11111111-1111-4111-8111-111111111111'
print(json.dumps({'type':'thread.started','thread_id':thread}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'Full response to '+prompt}}))
print('diagnostic',file=sys.stderr)
pathlib.Path(args[1],'unwanted-change').write_text('chat must not persist this')
"#).await.unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
    let controller = Controller::new(
        paths.clone(),
        worker.to_string_lossy().into_owned(),
        Duration::from_secs(3600),
    );
    let settings = Settings {
        model: "test-model".into(),
        reasoning: "high".into(),
        timeout_seconds: 1800,
        chat_progress: true,
    };
    kurbis_ai_protocol::write_json(&paths.plugin_data().join("settings.json"), &settings).unwrap();
    async fn submit(
        controller: &Controller,
        paths: &RuntimePaths,
        session_id: Option<Uuid>,
        plugin: &str,
        cancelled: bool,
    ) -> JobResult {
        let job = JobRequest {
            id: Uuid::new_v4(),
            session_id,
            plugin: plugin.into(),
            action: JobAction::Ask,
            prompt: "remember kiwi".into(),
            player: "Operator".into(),
            operator_uuid: Some(Uuid::new_v4()),
            created_unix_ms: now_ms(),
        };
        if cancelled {
            fs::write(paths.cancel().join(format!("{}.json", job.id)), "{}")
                .await
                .unwrap();
        }
        write_json(
            &paths.operator_authorizations(),
            &OperatorAuthorizationSnapshot {
                updated_unix_ms: now_ms(),
                operator_uuids: vec![job.operator_uuid.unwrap()],
            },
        )
        .unwrap();
        write_json(&paths.requests().join(format!("{}.json", job.id)), &job).unwrap();
        let _ = controller.run_once().await;
        serde_json::from_slice(
            &fs::read(paths.results().join(format!("{}.json", job.id)))
                .await
                .unwrap(),
        )
        .unwrap()
    }
    let first = submit(&controller, &paths, None, "demo", false).await;
    assert_eq!(first.state, JobState::Completed);
    assert!(!paths.installed_plugin("demo").exists());
    assert!(
        !paths
            .plugin_workspace("demo")
            .join("unwanted-change")
            .exists()
    );
    let sessions_path = paths.plugin_data().join("sessions.json");
    let sessions: Sessions = kurbis_ai_protocol::read_json(&sessions_path).unwrap();
    let thread = sessions.sessions[0].thread_id.unwrap();
    assert_eq!(sessions.sessions[0].id, first.id);
    let second = submit(&controller, &paths, Some(first.id), "demo", false).await;
    assert_eq!(second.state, JobState::Completed);
    let calls = fs::read_to_string(temp.path().join("calls.jsonl"))
        .await
        .unwrap();
    let calls: Vec<serde_json::Value> = calls
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(calls.len(), 2);
    let args = calls[1]["args"].as_array().unwrap();
    assert!(args.contains(&serde_json::json!("read-only")));
    assert!(args.contains(&serde_json::json!("test-model")));
    assert!(args.contains(&serde_json::json!("model_reasoning_effort=\"high\"")));
    let exec = args.iter().position(|a| a == "exec").unwrap();
    let config_positions: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| **a == "-c")
        .map(|(index, _)| index)
        .collect();
    assert_eq!(config_positions.len(), 4);
    assert!(
        config_positions.iter().all(|index| *index < exec),
        "exec-level config drops global network override"
    );
    assert!(args.contains(&serde_json::json!(
        "sandbox_workspace_write.network_access=true"
    )));
    assert!(args.contains(&serde_json::json!("--ignore-user-config")));
    assert!(
        args.iter()
            .any(|argument| argument
                .as_str()
                .is_some_and(|value| value.contains("mcp_servers.kurbis_ai")
                    && value.contains("required=true")))
    );
    let resume = args.iter().position(|a| a == "resume").unwrap();
    assert_eq!(args[resume + 1], thread.to_string());
    let sessions: Sessions = kurbis_ai_protocol::read_json(&sessions_path).unwrap();
    assert_eq!(sessions.sessions.len(), 1);
    assert_eq!(sessions.sessions[0].turns.len(), 2);
    assert_eq!(sessions.sessions[0].thread_id, Some(thread));
    let invalid = submit(&controller, &paths, Some(Uuid::new_v4()), "demo", false).await;
    assert_eq!(invalid.state, JobState::Failed);
    let mismatch = submit(&controller, &paths, Some(first.id), "other", false).await;
    assert_eq!(mismatch.state, JobState::Failed);
    let cancelled = submit(&controller, &paths, None, "demo", true).await;
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert_eq!(
        fs::read_to_string(temp.path().join("calls.jsonl"))
            .await
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[tokio::test]
async fn request_queue_orders_by_creation_time_instead_of_random_uuid() {
    let temp = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::new(temp.path());
    paths.create_dirs().await.unwrap();
    let controller = Controller::new(paths.clone(), "unused".into(), Duration::from_secs(1));
    let mut oldest = None;
    for (id, created) in [(Uuid::nil(), 20), (Uuid::from_u128(u128::MAX), 10)] {
        let request = JobRequest {
            id,
            session_id: None,
            action: JobAction::Ask,
            plugin: "demo".into(),
            prompt: "hi".into(),
            player: "Op".into(),
            operator_uuid: Some(Uuid::new_v4()),
            created_unix_ms: created,
        };
        let path = paths.requests().join(format!("{id}.json"));
        write_json(&path, &request).unwrap();
        if created == 10 {
            oldest = Some(path);
        }
    }
    assert_eq!(controller.next_request().await.unwrap(), oldest);
}

#[tokio::test]
async fn interrupted_jobs_are_not_replayed_and_their_threads_remain_resumable() {
    let temp = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::new(temp.path());
    paths.create_dirs().await.unwrap();
    let controller = Controller::new(paths.clone(), "must-not-run".into(), Duration::from_secs(1));
    let plugin = controller.ensure_plugin_workspace("demo").await.unwrap();
    fs::write(plugin.join("uncommitted"), "partial change")
        .await
        .unwrap();
    let job = JobRequest {
        id: Uuid::new_v4(),
        session_id: None,
        action: JobAction::Ask,
        plugin: "demo".into(),
        prompt: "hi".into(),
        player: "Op".into(),
        operator_uuid: Some(Uuid::new_v4()),
        created_unix_ms: 0,
    };
    write_json(&paths.processing().join(format!("{}.json", job.id)), &job).unwrap();
    let thread = Uuid::new_v4();
    write_json(
        &paths.plugin_data().join("sessions.json"),
        &Sessions {
            sessions: vec![Session {
                id: job.id,
                thread_id: None,
                operator_uuid: job.operator_uuid.unwrap(),
                plugin: job.plugin.clone(),
                player: job.player.clone(),
                action: job.action.clone(),
                title: job.prompt.clone(),
                turns: vec![Turn {
                    job_id: job.id,
                    prompt: job.prompt.clone(),
                }],
                updated_unix_ms: 0,
            }],
        },
    )
    .unwrap();
    let mut recorder = crate::progress::Recorder::new(&paths.plugin_data(), job.id).unwrap();
    recorder
        .line(
            &format!(r#"{{"type":"thread.started","thread_id":"{thread}"}}"#),
            false,
        )
        .unwrap();
    drop(recorder);
    controller.recover_processing().await.unwrap();
    assert!(controller.next_request().await.unwrap().is_none());
    assert!(!plugin.join("uncommitted").exists());
    let sessions: Sessions =
        kurbis_ai_protocol::read_json(&paths.plugin_data().join("sessions.json")).unwrap();
    assert_eq!(sessions.sessions[0].thread_id, Some(thread));
    assert_eq!(sessions.sessions[0].turns.len(), 1);
    let result: JobResult = serde_json::from_slice(
        &fs::read(paths.results().join(format!("{}.json", job.id)))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result.state, JobState::Failed);
    assert!(result.message.contains("restarted"));
}

#[tokio::test]
async fn queued_jobs_are_cancelled_when_operator_permission_is_revoked() {
    let temp = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::new(temp.path().join("runtime"));
    paths.create_dirs().await.unwrap();
    let controller = Controller::new(paths.clone(), "must-not-run".into(), Duration::from_secs(1));
    let operator = Uuid::new_v4();
    let request = JobRequest {
        id: Uuid::new_v4(),
        session_id: None,
        action: JobAction::Ask,
        plugin: "server-control".into(),
        prompt: "spawn a pig".into(),
        player: "Operator".into(),
        operator_uuid: Some(operator),
        created_unix_ms: now_ms(),
    };
    write_json(
        &paths.operator_authorizations(),
        &OperatorAuthorizationSnapshot {
            updated_unix_ms: now_ms(),
            operator_uuids: vec![],
        },
    )
    .unwrap();
    write_json(
        &paths.requests().join(format!("{}.json", request.id)),
        &request,
    )
    .unwrap();
    assert!(controller.run_once().await.unwrap());
    let result: JobResult = serde_json::from_slice(
        &fs::read(paths.results().join(format!("{}.json", request.id)))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result.state, JobState::Cancelled);
    assert!(result.message.contains("permission was revoked"));
}

#[tokio::test]
async fn reset_removes_model_created_nested_git_metadata_before_controller_git_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let paths = RuntimePaths::new(temp.path().join("runtime"));
    paths.create_dirs().await.unwrap();
    let controller = Controller::new(paths.clone(), "unused".into(), Duration::from_secs(1));
    let plugin = controller.ensure_plugin_workspace("demo").await.unwrap();
    let nested_git = plugin.join("nested/.git/hooks");
    fs::create_dir_all(&nested_git).await.unwrap();
    fs::write(nested_git.join("pre-commit"), "#!/bin/sh\nexit 99\n")
        .await
        .unwrap();
    reset_plugin_workspace(&plugin).await.unwrap();
    assert!(!plugin.join("nested/.git").exists());
}
