use std::{path::Path, process::Stdio, time::Duration};

use anyhow::Context;
use tokio::{fs, process::Command, time};
use uuid::Uuid;

use crate::{paths::RuntimePaths, progress::Recorder};
use tokio::io::{AsyncBufReadExt, BufReader};
use vibe_protocol::{JobAction, JobRequest, Settings};

#[derive(Clone, Debug)]
pub struct CodexRunner {
    executable: String,
    timeout: Duration,
}

#[derive(Debug)]
pub enum CodexOutcome {
    Completed,
    Cancelled,
}

impl CodexRunner {
    pub fn new(executable: impl Into<String>, timeout: Duration) -> Self {
        Self {
            executable: executable.into(),
            timeout,
        }
    }

    pub async fn run(
        &self,
        paths: &RuntimePaths,
        job: &JobRequest,
        plugin_dir: &Path,
        prompt: &str,
        resume: Option<Uuid>,
        settings: &Settings,
    ) -> anyhow::Result<CodexOutcome> {
        let job_id = job.id;
        let read_only = job.action == JobAction::Chat;
        let mut recorder = Recorder::new(&paths.bridge_data(), job_id)?;
        recorder.step(if resume.is_some() {
            "Resuming Codex session"
        } else {
            "Starting Codex session"
        })?;
        let mut command = Command::new(&self.executable);
        command
            .arg("-C")
            .arg(plugin_dir)
            .arg("-s")
            .arg(if read_only {
                "read-only"
            } else {
                "workspace-write"
            })
            .arg("-c")
            .arg("sandbox_workspace_write.network_access=true")
            .arg("-a")
            .arg("never")
            .arg("exec")
            .arg("--json");
        if let Some(thread) = resume {
            command.arg("resume").arg(thread.to_string());
        }
        command
            .arg("-c")
            .arg(format!("model_reasoning_effort={:?}", settings.reasoning));
        if !settings.model.is_empty() {
            command.arg("-m").arg(&settings.model);
        }
        command
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command
            .spawn()
            .with_context(|| format!("spawn {}", self.executable))?;

        let _group = ProcessGroup(child.id());

        {
            let mut stdin = child.stdin.take().context("Codex stdin unavailable")?;
            use tokio::io::AsyncWriteExt;
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await?;
        }

        let mut stdout =
            BufReader::new(child.stdout.take().context("Codex stdout unavailable")?).lines();
        let mut stderr =
            BufReader::new(child.stderr.take().context("Codex stderr unavailable")?).lines();
        let cancel_path = paths.cancel().join(format!("{job_id}.json"));
        let mut out_open = true;
        let mut err_open = true;
        let deadline = time::Instant::now()
            + self
                .timeout
                .min(Duration::from_secs(settings.timeout_seconds));
        let mut status = None;
        let outcome = loop {
            tokio::select! {
                line = stdout.next_line(), if out_open => {
                    match line? {
                        Some(line) => recorder.line(&line, false)?,
                        None => out_open = false,
                    }
                }
                line = stderr.next_line(), if err_open => {
                    match line? {
                        Some(line) => recorder.line(&line, true)?,
                        None => err_open = false,
                    }
                }
                exited = child.wait(), if status.is_none() => {
                    status = Some(exited?);
                }
                _ = time::sleep(Duration::from_millis(100)) => {
                    if fs::try_exists(&cancel_path).await? {
                        terminate_child(&mut child).await?;
                        let _ = fs::remove_file(&cancel_path).await;
                        recorder.step("Codex process group cancelled")?;
                        break Ok(CodexOutcome::Cancelled);
                    }
                    if time::Instant::now() >= deadline {
                        terminate_child(&mut child).await?;
                        recorder.step("Codex job timed out; process group terminated")?;
                        break Err(anyhow::anyhow!("Codex job timed out"));
                    }
                }
            }
            if let Some(status) = status
                && !out_open
                && !err_open
            {
                recorder.step(&format!("Codex exited with {status}"))?;
                break if status.success() {
                    Ok(CodexOutcome::Completed)
                } else {
                    Err(anyhow::anyhow!("Codex exited with {status}"))
                };
            }
        };
        if matches!(outcome, Ok(CodexOutcome::Completed))
            && let Some(expected) = resume
        {
            let progress: vibe_protocol::Progress = vibe_protocol::read_json(
                &paths
                    .bridge_data()
                    .join("progress")
                    .join(format!("{job_id}.json")),
            )?;
            anyhow::ensure!(
                progress.thread_id == Some(expected),
                "Codex did not resume the requested thread"
            );
        }
        outcome.map_err(|error| {
            let progress: vibe_protocol::Progress = vibe_protocol::read_json(
                &paths
                    .bridge_data()
                    .join("progress")
                    .join(format!("{job_id}.json")),
            )
            .unwrap_or_default();
            if let Some(event) = progress
                .events
                .iter()
                .rev()
                .find(|e| e.kind == "error" || e.kind == "turn.failed")
            {
                error.context(event.message.clone())
            } else {
                error
            }
        })
    }
}

struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", &format!("-{pid}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

async fn terminate_child(child: &mut tokio::process::Child) -> anyhow::Result<()> {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let group = format!("-{pid}");
        let _ = Command::new("/bin/kill")
            .arg("-KILL")
            .arg(group)
            .status()
            .await;
    }

    if child.try_wait()?.is_none() {
        child.kill().await?;
    }
    let _ = child.wait().await;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[tokio::test]
    async fn timeout_kills_the_entire_codex_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = RuntimePaths::new(temp.path().join("runtime"));
        paths.create_dirs().await.expect("runtime dirs");
        let plugin_dir = paths.plugin_workspace("demo");
        fs::create_dir_all(&plugin_dir).await.expect("plugin dir");

        let fake_codex = temp.path().join("fake-codex.sh");
        fs::write(
            &fake_codex,
            r#"#!/bin/sh
plugin_dir="$2"
sleep 30 &
echo "$!" > "$plugin_dir/child.pid"
wait
"#,
        )
        .await
        .expect("fake codex");
        let mut permissions = std::fs::metadata(&fake_codex)
            .expect("fake codex metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_codex, permissions).expect("make fake codex executable");

        let runner = CodexRunner::new(fake_codex.to_string_lossy(), Duration::from_millis(300));
        let error = runner
            .run(
                &paths,
                &JobRequest {
                    id: Uuid::new_v4(),
                    session_id: None,
                    action: JobAction::Code,
                    plugin: "demo".into(),
                    prompt: "test prompt".into(),
                    player: "Tester".into(),
                    created_unix_ms: 0,
                },
                &plugin_dir,
                "test prompt",
                None,
                &Settings::default(),
            )
            .await
            .expect_err("job should time out");
        assert!(error.to_string().contains("timed out"));

        let child_pid = fs::read_to_string(plugin_dir.join("child.pid"))
            .await
            .expect("child pid");
        let child_pid = child_pid.trim();
        for _ in 0..20 {
            let status = Command::new("/bin/kill")
                .args(["-0", child_pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .expect("check child process");
            if !status.success() {
                return;
            }
            time::sleep(Duration::from_millis(50)).await;
        }

        panic!("Codex child process {child_pid} survived timeout");
    }
    #[tokio::test]
    async fn detects_wrong_resume_thread_and_keeps_full_output() {
        let temp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::new(temp.path().join("runtime"));
        paths.create_dirs().await.unwrap();
        let worker = temp.path().join("worker");
        fs::write(&worker, r#"#!/usr/bin/env python3
import json,sys
sys.stdin.read()
print(json.dumps({'type':'thread.started','thread_id':'22222222-2222-4222-8222-222222222222'}))
for i in range(200):
 print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':str(i)+'-'+'X'*4000}}))
print('last stderr line',file=sys.stderr)
"#).await.unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let job = JobRequest {
            id: Uuid::new_v4(),
            session_id: None,
            action: JobAction::Chat,
            plugin: "demo".into(),
            prompt: "hi".into(),
            player: "Op".into(),
            created_unix_ms: 0,
        };
        let runner = CodexRunner::new(worker.to_string_lossy(), Duration::from_secs(15));
        let error = runner
            .run(
                &paths,
                &job,
                temp.path(),
                "hi",
                Some(Uuid::nil()),
                &Settings::default(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("did not resume"));
        let text = fs::read_to_string(
            paths
                .bridge_data()
                .join("events")
                .join(format!("{}.jsonl", job.id)),
        )
        .await
        .unwrap();
        assert_eq!(text.lines().count(), 204);
        assert!(text.contains(&format!("199-{}", "X".repeat(4000))));
        assert!(text.contains("last stderr line"));
    }

    #[tokio::test]
    async fn active_cancellation_stops_the_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::new(temp.path().join("runtime"));
        paths.create_dirs().await.unwrap();
        let worker = temp.path().join("worker");
        fs::write(
            &worker,
            "#!/bin/sh\ncat >/dev/null\nsleep 30 &\necho $! > \"$2/child.pid\"\nwait\n",
        )
        .await
        .unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let job = JobRequest {
            id: Uuid::new_v4(),
            session_id: None,
            action: JobAction::Chat,
            plugin: "demo".into(),
            prompt: "hi".into(),
            player: "Op".into(),
            created_unix_ms: 0,
        };
        let cancel = paths.cancel().join(format!("{}.json", job.id));
        let trigger = tokio::spawn(async move {
            time::sleep(Duration::from_millis(300)).await;
            fs::write(cancel, "{}").await.unwrap();
        });
        let runner = CodexRunner::new(worker.to_string_lossy(), Duration::from_secs(15));
        assert!(matches!(
            runner
                .run(&paths, &job, temp.path(), "hi", None, &Settings::default())
                .await
                .unwrap(),
            CodexOutcome::Cancelled
        ));
        trigger.await.unwrap();
        let pid = fs::read_to_string(temp.path().join("child.pid"))
            .await
            .unwrap();
        time::sleep(Duration::from_millis(100)).await;
        let status = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .unwrap();
        assert!(!status.success());
    }
}
