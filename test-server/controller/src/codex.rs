use std::{path::Path, process::Stdio, time::Duration};

use anyhow::Context;
use tokio::{fs, process::Command, time};
use uuid::Uuid;

use crate::{paths::RuntimePaths, progress::Recorder};
use tokio::io::{AsyncBufReadExt, BufReader};
use vibe_protocol::Settings;

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
        job_id: Uuid,
        plugin_dir: &Path,
        prompt: &str,
        resume: Option<Uuid>,
        settings: &Settings,
        read_only: bool,
    ) -> anyhow::Result<CodexOutcome> {
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
        if outcome.is_ok()
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
        outcome
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
                Uuid::new_v4(),
                &plugin_dir,
                "test prompt",
                None,
                &Settings::default(),
                false,
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
}
