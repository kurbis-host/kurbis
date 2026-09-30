use super::*;

async fn runtime(root: &Path, worker: Uuid) -> NativeRuntime {
    NativeRuntime::new(
        PathBuf::from("/bin/sleep"),
        root.to_path_buf(),
        worker,
        "native-test-token-at-least-thirty-two-bytes".into(),
    )
    .await
    .unwrap()
}

async fn process(runtime: &NativeRuntime, server: Uuid, generation: i64) -> Child {
    let marker = format!("kurbis-native-{}", Uuid::new_v4().simple());
    let mut command = Command::new("/bin/sleep");
    command.as_std_mut().arg0(&marker);
    let child = command.arg("60").kill_on_drop(false).spawn().unwrap();
    let record = ProcessRecord {
        pid: child.id().unwrap(),
        generation,
        marker,
    };
    fs::create_dir_all(runtime.directory(server).join("run"))
        .await
        .unwrap();
    fs::write(
        runtime.record_path(server),
        serde_json::to_vec(&record).unwrap(),
    )
    .await
    .unwrap();
    child
}

#[tokio::test]
async fn actual_host_process_survives_backend_drop_and_is_recovered_without_a_second_copy() {
    let root = tempfile::tempdir().unwrap();
    let worker = Uuid::new_v4();
    let server = Uuid::new_v4();
    let original = runtime(root.path(), worker).await;
    let child = process(&original, server, 3).await;
    original.children.lock().await.insert(server, child);
    assert_eq!(original.server_generation(server).await.unwrap(), Some(3));
    drop(original);
    let recovered = runtime(root.path(), worker).await;
    assert_eq!(recovered.server_generation(server).await.unwrap(), Some(3));
    recovered.abort_start(server).await.unwrap();
    assert_eq!(recovered.server_generation(server).await.unwrap(), None);
    assert!(!recovered.record_path(server).exists());
    fs::remove_dir(recovered.sockets).await.unwrap();
}

#[tokio::test]
async fn stale_pid_metadata_cannot_signal_an_unrelated_host_process() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path(), Uuid::new_v4()).await;
    let server = Uuid::new_v4();
    let mut child = process(&runtime, server, 9).await;
    let mut record = runtime.record(server).await.unwrap().unwrap();
    record.marker = format!("kurbis-native-{}", Uuid::new_v4().simple());
    fs::write(
        runtime.record_path(server),
        serde_json::to_vec(&record).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(runtime.server_generation(server).await.unwrap(), None);
    runtime.abort_start(server).await.unwrap();
    assert!(child.try_wait().unwrap().is_none());
    child.kill().await.unwrap();
    fs::remove_dir(runtime.sockets).await.unwrap();
}

#[tokio::test]
async fn native_log_cursor_survives_worker_restart_and_waits_for_complete_lines() {
    let root = tempfile::tempdir().unwrap();
    let worker = Uuid::new_v4();
    let server = Uuid::new_v4();
    let original = runtime(root.path(), worker).await;
    let run = original.directory(server).join("run");
    fs::create_dir_all(&run).await.unwrap();
    fs::write(run.join("native.log"), "first\npartial")
        .await
        .unwrap();
    assert_eq!(original.logs_since(server, "").await.unwrap(), ["first"]);
    assert!(original.logs_since(server, "").await.unwrap().is_empty());
    drop(original);
    let recovered = runtime(root.path(), worker).await;
    fs::write(run.join("native.log"), "first\npartial completed\n")
        .await
        .unwrap();
    assert_eq!(
        recovered.logs_since(server, "").await.unwrap(),
        ["partial completed"]
    );
    assert!(recovered.logs_since(server, "").await.unwrap().is_empty());
    fs::write(run.join("native.log"), "reset\n").await.unwrap();
    assert_eq!(recovered.logs_since(server, "").await.unwrap(), ["reset"]);
    let mut oversized = "x".repeat(256 * 1024 + 12);
    oversized.push_str("\nnext\n");
    fs::write(run.join("native.log"), "").await.unwrap();
    assert!(recovered.logs_since(server, "").await.unwrap().is_empty());
    fs::write(run.join("native.log"), oversized).await.unwrap();
    assert_eq!(
        recovered.logs_since(server, "").await.unwrap()[0].len(),
        256 * 1024
    );
    assert_eq!(
        recovered.logs_since(server, "").await.unwrap(),
        ["xxxxxxxxxxxx", "next"]
    );
    fs::remove_dir(recovered.sockets).await.unwrap();
}
