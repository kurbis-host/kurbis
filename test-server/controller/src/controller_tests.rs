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
            "user.name=Pumpkin Vibe Test",
            "-c",
            "user.email=pumpkin-vibe-test@localhost",
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
            "user.name=Pumpkin Vibe Test",
            "-c",
            "user.email=pumpkin-vibe-test@localhost",
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
            created_unix_ms: now_ms(),
        };
        atomic_json_write(
            &paths.requests().join(format!("{}.json", request.id)),
            &request,
        )
        .await
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
    assert!(first.message.contains("restart Pumpkin to activate"));
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
    assert!(reverted.message.contains("restart Pumpkin to activate"));
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
