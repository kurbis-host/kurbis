use std::{path::Path, process::Stdio, time::Duration};

use anyhow::Context;
use tokio::{fs, process::Command, time};
use uuid::Uuid;

use crate::{model::MAX_LOG_BYTES, paths::RuntimePaths};

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
    ) -> anyhow::Result<CodexOutcome> {
        let log_path = plugin_dir.join(".vibe-codex.log");
        let log_file = std::fs::File::create(&log_path)
            .with_context(|| format!("create {}", log_path.display()))?;
        let log_err = log_file.try_clone()?;

        let mut command = Command::new(&self.executable);
        command
            .arg("-C")
            .arg(plugin_dir)
            .arg("-s")
            .arg("workspace-write")
            .arg("-c")
            .arg("sandbox_workspace_write.network_access=true")
            .arg("-a")
            .arg("never")
            .arg("exec")
            .arg("--json")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_err))
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command
            .spawn()
            .with_context(|| format!("spawn {}", self.executable))?;

        {
            let mut stdin = child.stdin.take().context("Codex stdin unavailable")?;
            use tokio::io::AsyncWriteExt;
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await?;
        }

        let cancel_path = paths.cancel().join(format!("{job_id}.json"));
        let wait = async {
            loop {
                tokio::select! {
                    status = child.wait() => {
                        let status = status?;
                        anyhow::ensure!(status.success(), "Codex exited with {status}");
                        return Ok(CodexOutcome::Completed);
                    }
                    _ = time::sleep(Duration::from_millis(200)) => {
                        if fs::try_exists(&cancel_path).await? {
                            terminate_child(&mut child).await?;
                            let _ = fs::remove_file(&cancel_path).await;
                            return Ok(CodexOutcome::Cancelled);
                        }
                    }
                }
            }
        };

        let outcome = match time::timeout(self.timeout, wait).await {
            Ok(outcome) => outcome,
            Err(_) => {
                terminate_child(&mut child).await?;
                Err(anyhow::anyhow!("Codex job timed out"))
            }
        };
        trim_log(&log_path).await?;
        outcome
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

async fn trim_log(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::metadata(path).await?;
    if metadata.len() <= MAX_LOG_BYTES as u64 {
        return Ok(());
    }
    let bytes = fs::read(path).await?;
    let start = bytes.len().saturating_sub(MAX_LOG_BYTES);
    fs::write(path, &bytes[start..]).await?;
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
            .run(&paths, Uuid::new_v4(), &plugin_dir, "test prompt")
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
