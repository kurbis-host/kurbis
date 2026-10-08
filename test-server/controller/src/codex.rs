use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::Context;
use tokio::{fs, process::Command, time};
use uuid::Uuid;

use crate::{paths::RuntimePaths, progress::Recorder};

const MAX_CODEX_LINE_BYTES: usize = 128 * 1024;
use kurbis_ai_protocol::{
    JobAction, JobRequest, McpActionResult, PrivateAuthMessage, PrivateAuthStatus, Settings,
    now_ms, write_json,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

#[derive(Clone, Debug)]
pub struct CodexRunner {
    executable: String,
    timeout: Duration,
    codex_home: PathBuf,
    controller_executable: PathBuf,
}

#[derive(Debug)]
pub enum CodexOutcome {
    Completed,
    Cancelled,
}

impl CodexRunner {
    pub fn new(
        executable: impl Into<String>,
        timeout: Duration,
        codex_home: PathBuf,
        controller_executable: PathBuf,
    ) -> Self {
        Self {
            executable: executable.into(),
            timeout,
            codex_home,
            controller_executable,
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
        let session = job.session_id.unwrap_or(job.id);
        let job_home = self.prepare_session_home(paths, session).await?;
        self.run_with_home(paths, job, plugin_dir, prompt, resume, settings, &job_home)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_with_home(
        &self,
        paths: &RuntimePaths,
        job: &JobRequest,
        plugin_dir: &Path,
        prompt: &str,
        resume: Option<Uuid>,
        settings: &Settings,
        job_home: &Path,
    ) -> anyhow::Result<CodexOutcome> {
        secure_codex_home(job_home).await?;
        let job_id = job.id;
        let read_only = job.action == JobAction::Ask;
        let mut recorder = Recorder::new(&paths.plugin_data(), job_id)?;
        recorder.step(if resume.is_some() {
            "Resuming Codex session"
        } else {
            "Starting Codex session"
        })?;
        let mut command = Command::new(&self.executable);
        command
            .env("CODEX_HOME", job_home)
            .env("KURBIS_AI_LOGIN_JOB", job.id.to_string())
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_ACCESS_TOKEN")
            .arg("-C")
            .arg(plugin_dir)
            .arg("--ignore-user-config")
            .arg("--sandbox")
            .arg(if read_only {
                "read-only"
            } else {
                "workspace-write"
            })
            .arg("-c")
            .arg("cli_auth_credentials_store=\"file\"")
            .arg("-c")
            .arg("sandbox_workspace_write.network_access=true")
            // All overrides stay before `exec`; exec-local config can replace
            // the root override set and silently discard sibling settings.
            .arg("-c")
            .arg(format!("model_reasoning_effort=\"{}\"", settings.reasoning));
        if read_only {
            command
                .arg("--disable")
                .arg("shell_tool")
                .arg("--disable")
                .arg("unified_exec");
            let config = mcp_server_config(&self.controller_executable, paths.root(), job.id);
            command.arg("-c").arg(config);
        }
        command.arg("exec").arg("--json");
        if let Some(thread) = resume {
            command.arg("resume").arg(thread.to_string());
        }
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

        let mut stdout = BufReader::new(child.stdout.take().context("Codex stdout unavailable")?);
        let mut stderr = BufReader::new(child.stderr.take().context("Codex stderr unavailable")?);
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
                line = read_bounded_line(&mut stdout, MAX_CODEX_LINE_BYTES), if out_open => {
                    match line? {
                        Some(line) => recorder.line(&line, false)?,
                        None => out_open = false,
                    }
                }
                line = read_bounded_line(&mut stderr, MAX_CODEX_LINE_BYTES), if err_open => {
                    match line? {
                        Some(line) => recorder.line(&line, true)?,
                        None => err_open = false,
                    }
                }
                exited = child.wait(), if status.is_none() => {
                    status = Some(exited?);
                }
                _ = time::sleep(Duration::from_millis(100)) => {
                    if status.is_none() && fs::try_exists(&cancel_path).await? {
                        eprintln!("device login cancellation marker observed for {}", job.id);
                        terminate_child(&mut child).await?;
                        let _ = fs::remove_file(&cancel_path).await;
                        recorder.step("Codex process group cancelled")?;
                        break Ok(CodexOutcome::Cancelled);
                    }
                    drain_mcp_audit(paths, job_id, &mut recorder).await?;
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
        drain_mcp_audit(paths, job_id, &mut recorder).await?;
        if matches!(outcome, Ok(CodexOutcome::Completed))
            && let Some(expected) = resume
        {
            let progress: kurbis_ai_protocol::Progress = kurbis_ai_protocol::read_json(
                &paths
                    .plugin_data()
                    .join("progress")
                    .join(format!("{job_id}.json")),
            )?;
            anyhow::ensure!(
                progress.thread_id == Some(expected),
                "Codex did not resume the requested thread"
            );
        }
        outcome.map_err(|error| {
            let progress: kurbis_ai_protocol::Progress = kurbis_ai_protocol::read_json(
                &paths
                    .plugin_data()
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

    async fn prepare_session_home(
        &self,
        paths: &RuntimePaths,
        session: Uuid,
    ) -> anyhow::Result<PathBuf> {
        secure_codex_home(&self.codex_home).await?;
        let home = paths.codex_session_home(session);
        secure_codex_home(&home).await?;
        let credentials = self.codex_home.join("auth.json");
        if fs::try_exists(&credentials).await? {
            let metadata = fs::symlink_metadata(&credentials).await?;
            anyhow::ensure!(
                metadata.file_type().is_file(),
                "shared Codex credentials must be a regular file"
            );
            let destination = home.join("auth.json");
            if let Ok(metadata) = fs::symlink_metadata(&destination).await {
                anyhow::ensure!(
                    metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
                    "isolated Codex credentials must be a regular file"
                );
            }
            let temporary = home.join(format!(".auth-{}.tmp", Uuid::new_v4()));
            fs::copy(&credentials, &temporary).await?;
            fs::rename(&temporary, &destination).await?;
            secure_credential_file(&home).await;
        }
        Ok(home)
    }

    pub async fn login_with_device_code(
        &self,
        paths: &RuntimePaths,
        job: &JobRequest,
    ) -> anyhow::Result<bool> {
        let operator_uuid = job
            .operator_uuid
            .context("operator identity is required for device login")?;
        secure_codex_home(&self.codex_home).await?;
        clear_login_log(&self.codex_home).await;

        let mut command = Command::new(&self.executable);
        command
            .env("CODEX_HOME", &self.codex_home)
            .env("KURBIS_AI_LOGIN_JOB", job.id.to_string())
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_ACCESS_TOKEN")
            .arg("-c")
            .arg("cli_auth_credentials_store=\"file\"")
            .arg("login")
            .arg("--device-auth")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().context("start Codex device login")?;
        let _group = ProcessGroup(child.id());
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .context("Codex login stdout unavailable")?,
        );
        let mut stderr = BufReader::new(
            child
                .stderr
                .take()
                .context("Codex login stderr unavailable")?,
        );
        let cancel_path = paths.cancel().join(format!("{}.json", job.id));
        let _ = fs::remove_file(&cancel_path).await;
        let deadline = time::Instant::now() + self.timeout.min(Duration::from_secs(600));
        let mut out_open = true;
        let mut err_open = true;
        let mut status = None;
        let mut verification_url = None;
        let mut user_code = None;
        let mut details_sent = false;
        let result = loop {
            tokio::select! {
                line = read_bounded_line(&mut stdout, MAX_CODEX_LINE_BYTES), if out_open => {
                    match line {
                        Ok(Some(line)) => {
                            capture_device_details(&line, &mut verification_url, &mut user_code);
                        }
                        Ok(None) => out_open = false,
                        Err(error) => break Err(error.into()),
                    }
                }
                line = read_bounded_line(&mut stderr, MAX_CODEX_LINE_BYTES), if err_open => {
                    match line {
                        Ok(Some(line)) => {
                            capture_device_details(&line, &mut verification_url, &mut user_code);
                        }
                        Ok(None) => err_open = false,
                        Err(error) => break Err(error.into()),
                    }
                }
                exited = child.wait(), if status.is_none() => {
                    status = Some(exited?);
                }
                _ = time::sleep(Duration::from_millis(100)) => {
                    if !details_sent && verification_url.is_some() && user_code.is_some() {
                        deliver_device_code(
                            paths,
                            job,
                            operator_uuid,
                            verification_url.clone(),
                            user_code.clone(),
                            PrivateAuthStatus::CodeAvailable,
                        )
                        .await?;
                        details_sent = true;
                    }
                    if status.is_none() && fs::try_exists(&cancel_path).await? {
                        terminate_child(&mut child).await?;
                        let _ = fs::remove_file(&cancel_path).await;
                        break Ok(false);
                    }
                    if time::Instant::now() >= deadline {
                        terminate_child(&mut child).await?;
                        break Err(anyhow::anyhow!("Codex device login timed out"));
                    }
                }
            }
            if let Some(status) = status
                && !out_open
                && !err_open
            {
                if status.success() {
                    break Ok(true);
                }
                break Err(anyhow::anyhow!("Codex device login exited with {status}"));
            }
        };

        clear_login_log(&self.codex_home).await;
        secure_credential_file(&self.codex_home).await;
        match result {
            Ok(true) => {
                if !details_sent {
                    if verification_url.is_some() && user_code.is_some() {
                        deliver_device_code(
                            paths,
                            job,
                            operator_uuid,
                            verification_url,
                            user_code,
                            PrivateAuthStatus::CodeAvailable,
                        )
                        .await?;
                    } else {
                        deliver_device_code(
                            paths,
                            job,
                            operator_uuid,
                            None,
                            None,
                            PrivateAuthStatus::Failed,
                        )
                        .await?;
                    }
                } else {
                    deliver_device_code(
                        paths,
                        job,
                        operator_uuid,
                        None,
                        None,
                        PrivateAuthStatus::Completed,
                    )
                    .await?;
                }
                Ok(true)
            }
            Ok(false) => {
                // A terminal cancellation must be delivered even when the
                // one-time code was already shown, otherwise the player can
                // wait forever for a login that no longer exists.
                deliver_device_code(
                    paths,
                    job,
                    operator_uuid,
                    None,
                    None,
                    PrivateAuthStatus::Cancelled,
                )
                .await?;
                Ok(false)
            }
            Err(error) => {
                // Likewise, a non-zero exit, timeout, or malformed terminal
                // stream always gets an explicit private terminal status.
                deliver_device_code(
                    paths,
                    job,
                    operator_uuid,
                    None,
                    None,
                    PrivateAuthStatus::Failed,
                )
                .await?;
                Err(error)
            }
        }
    }
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(limit.min(4096));
    let mut truncated = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            if bytes.is_empty() && !truncated {
                return Ok(None);
            }
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
        if let Some(newline) = chunk.iter().position(|byte| *byte == b'\n') {
            if !truncated {
                let remaining = limit.saturating_sub(bytes.len());
                if newline > remaining {
                    bytes.extend_from_slice(&chunk[..remaining]);
                } else {
                    bytes.extend_from_slice(&chunk[..newline]);
                }
            }
            reader.consume(newline + 1);
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
        if !truncated {
            let remaining = limit.saturating_sub(bytes.len());
            if chunk.len() > remaining {
                bytes.extend_from_slice(&chunk[..remaining]);
                truncated = true;
            } else {
                bytes.extend_from_slice(chunk);
            }
        }
        let consumed = chunk.len();
        reader.consume(consumed);
    }
}

async fn drain_mcp_audit(
    paths: &RuntimePaths,
    job: Uuid,
    recorder: &mut Recorder,
) -> anyhow::Result<()> {
    let mut entries = match fs::read_dir(paths.mcp_audit()).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut processed = 0;
    while let Some(entry) = entries.next_entry().await? {
        if processed >= 16 || entry.path().extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        processed += 1;
        let path = entry.path();
        let bytes = match fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let result = serde_json::from_slice::<McpActionResult>(&bytes);
        let _ = fs::remove_file(&path).await;
        if let Ok(result) = result
            && result.job_id == job
        {
            record_mcp_audit(recorder, result)?;
        }
    }
    Ok(())
}

async fn secure_codex_home(home: &Path) -> anyhow::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(home).await {
        anyhow::ensure!(
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
            "Codex home must be a real directory"
        );
    }
    fs::create_dir_all(home).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).await?;
        let log_dir = home.join("log");
        if let Ok(metadata) = fs::symlink_metadata(&log_dir).await {
            anyhow::ensure!(
                metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
                "Codex log directory must be a real directory"
            );
        }
        fs::create_dir_all(&log_dir).await?;
        fs::set_permissions(log_dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(())
}

async fn secure_credential_file(home: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [home.join("auth.json"), home.join("config.toml")] {
            if let Ok(metadata) = fs::symlink_metadata(&path).await
                && metadata.file_type().is_file()
                && !metadata.file_type().is_symlink()
            {
                let _ = fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
            }
        }
    }
}

async fn clear_login_log(home: &Path) {
    let _ = fs::remove_file(home.join("log/codex-login.log")).await;
}

async fn deliver_device_code(
    paths: &RuntimePaths,
    job: &JobRequest,
    player_uuid: Uuid,
    verification_url: Option<String>,
    user_code: Option<String>,
    status: PrivateAuthStatus,
) -> anyhow::Result<()> {
    let message = PrivateAuthMessage {
        job_id: job.id,
        player_uuid,
        player_name: job.player.clone(),
        status,
        verification_url,
        user_code,
        expires_unix_ms: now_ms() + 120_000,
    };
    write_json(
        &paths
            .plugin_data()
            .join("private/auth")
            .join(format!("{}.json", job.id)),
        &message,
    )
}

fn capture_device_details(
    line: &str,
    verification_url: &mut Option<String>,
    user_code: &mut Option<String>,
) {
    if verification_url.is_none() {
        *verification_url = line.split_whitespace().find_map(|part| {
            let candidate = part.trim_matches(|c: char| {
                matches!(
                    c,
                    '(' | ')' | '[' | ']' | '<' | '>' | ',' | '.' | '\'' | '"'
                )
            });
            (candidate.starts_with("https://auth.openai.com/")).then(|| candidate.to_owned())
        });
    }
    if user_code.is_none() && line.to_ascii_lowercase().contains("code") {
        *user_code = line.split_whitespace().find_map(|part| {
            let candidate = part.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
            is_device_code(candidate).then(|| candidate.to_ascii_uppercase())
        });
    }
}

fn is_device_code(value: &str) -> bool {
    if let Some((left, right)) = value.split_once('-') {
        (4..=6).contains(&left.len())
            && (4..=6).contains(&right.len())
            && left
                .bytes()
                .chain(right.bytes())
                .all(|b| b.is_ascii_alphanumeric())
            && !right.contains('-')
    } else {
        (8..=12).contains(&value.len())
            && value.bytes().any(|byte| byte.is_ascii_digit())
            && value.bytes().any(|byte| byte.is_ascii_alphabetic())
            && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }
}

fn mcp_server_config(executable: &Path, runtime: &Path, job: Uuid) -> String {
    let executable = serde_json::to_string(&executable.to_string_lossy())
        .unwrap_or_else(|_| "\"kurbis-ai-controller\"".into());
    let runtime =
        serde_json::to_string(&runtime.to_string_lossy()).unwrap_or_else(|_| "\"runtime\"".into());
    format!(
        "mcp_servers.kurbis_ai={{command={executable},args=[\"--mcp-server\",\"--runtime\",{runtime},\"--job-id\",\"{job}\"],required=true}}"
    )
}

pub fn record_mcp_audit(recorder: &mut Recorder, result: McpActionResult) -> anyhow::Result<()> {
    let output = result.output.chars().take(1500).collect::<String>();
    recorder.step(&format!(
        "server action by {}: {}({}) => {}{}",
        result.operator_uuid,
        result.tool,
        serde_json::to_string(&result.arguments)?,
        output,
        if result.success { "" } else { " [failed]" },
    ))
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

#[cfg(test)]
mod device_auth_tests {
    use super::*;

    #[test]
    fn parses_only_expected_openai_device_urls_and_one_time_codes() {
        let mut url = None;
        let mut code = None;
        capture_device_details(
            "Open https://auth.openai.com/codex/device and enter this code: abcd-1234",
            &mut url,
            &mut code,
        );
        assert_eq!(url.as_deref(), Some("https://auth.openai.com/codex/device"));
        assert_eq!(code.as_deref(), Some("ABCD-1234"));
        let mut url = None;
        let mut code = None;
        capture_device_details(
            "Open https://evil.example/device and enter code abcd-1234",
            &mut url,
            &mut code,
        );
        assert!(url.is_none());
        assert_eq!(code.as_deref(), Some("ABCD-1234"));
    }

    #[tokio::test]
    async fn device_login_keeps_one_time_code_out_of_job_logs_and_seals_profile_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::new(temp.path().join("runtime"));
        paths.create_dirs().await.unwrap();
        let executable = temp.path().join("codex-login");
        fs::write(
            &executable,
            r#"#!/bin/sh
mkdir -p "$CODEX_HOME"
printf 'Open https://auth.openai.com/codex/device and enter code: ABCD-1234\n'
printf '{"access_token":"secret"}' > "$CODEX_HOME/auth.json"
"#,
        )
        .await
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let job = JobRequest {
            id: Uuid::new_v4(),
            session_id: None,
            action: JobAction::Login,
            plugin: "kurbis-ai".into(),
            prompt: String::new(),
            player: "Owner".into(),
            operator_uuid: Some(Uuid::new_v4()),
            created_unix_ms: now_ms(),
        };
        let home = paths.codex_home();
        let runner = CodexRunner::new(
            executable.to_string_lossy(),
            Duration::from_secs(5),
            home.clone(),
            PathBuf::from("/tmp/kurbis-ai-controller"),
        );
        assert!(runner.login_with_device_code(&paths, &job).await.unwrap());
        let delivery: PrivateAuthMessage = serde_json::from_slice(
            &fs::read(
                paths
                    .plugin_data()
                    .join("private/auth")
                    .join(format!("{}.json", job.id)),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(delivery.player_uuid, job.operator_uuid.unwrap());
        assert_eq!(delivery.user_code.as_deref(), Some("ABCD-1234"));
        assert_eq!(delivery.status, PrivateAuthStatus::CodeAvailable);
        assert!(
            !paths
                .plugin_data()
                .join("events")
                .join(format!("{}.jsonl", job.id))
                .exists()
        );
        assert!(
            !fs::read_to_string(home.join("log/codex-login.log"))
                .await
                .is_ok()
        );
        assert_eq!(
            std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(home.join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn failed_device_login_delivers_private_terminal_failure() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let paths = RuntimePaths::new(temp.path().join("runtime"));
        paths.create_dirs().await.unwrap();
        let executable = temp.path().join("codex-login-failed");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf 'login failed\\n' >&2\nexit 7\n",
        )
        .await
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let job = JobRequest {
            id: Uuid::new_v4(),
            session_id: None,
            action: JobAction::Login,
            plugin: "kurbis-ai".into(),
            prompt: String::new(),
            player: "Owner".into(),
            operator_uuid: Some(Uuid::new_v4()),
            created_unix_ms: now_ms(),
        };
        let runner = CodexRunner::new(
            executable.to_string_lossy(),
            Duration::from_secs(5),
            paths.codex_home(),
            PathBuf::from("/tmp/kurbis-ai-controller"),
        );
        let error = runner
            .login_with_device_code(&paths, &job)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exited"));
        let delivery: PrivateAuthMessage = serde_json::from_slice(
            &fs::read(
                paths
                    .plugin_data()
                    .join("private/auth")
                    .join(format!("{}.json", job.id)),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(delivery.status, PrivateAuthStatus::Failed);
    }

    #[test]
    fn per_job_mcp_config_contains_no_capability_secret_and_marks_server_required() {
        let config = mcp_server_config(
            Path::new("/tmp/controller"),
            Path::new("/tmp/runtime"),
            Uuid::nil(),
        );
        assert!(config.contains("required=true"));
        assert!(config.contains("--mcp-server"));
        assert!(config.contains("00000000-0000-0000-0000-000000000000"));
        assert!(!config.contains("token"));
    }
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

        let runner = CodexRunner::new(
            fake_codex.to_string_lossy(),
            Duration::from_secs(2),
            temp.path().join("codex-home"),
            PathBuf::from("/tmp/kurbis-ai-controller"),
        );
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
                    operator_uuid: None,
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
            action: JobAction::Ask,
            plugin: "demo".into(),
            prompt: "hi".into(),
            player: "Op".into(),
            operator_uuid: Some(Uuid::new_v4()),
            created_unix_ms: 0,
        };
        let runner = CodexRunner::new(
            worker.to_string_lossy(),
            Duration::from_secs(15),
            temp.path().join("codex-home"),
            PathBuf::from("/tmp/kurbis-ai-controller"),
        );
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
                .plugin_data()
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
            action: JobAction::Ask,
            plugin: "demo".into(),
            prompt: "hi".into(),
            player: "Op".into(),
            operator_uuid: Some(Uuid::new_v4()),
            created_unix_ms: 0,
        };
        let cancel = paths.cancel().join(format!("{}.json", job.id));
        let child_marker = temp.path().join("child.pid");
        let trigger = tokio::spawn(async move {
            let deadline = time::Instant::now() + Duration::from_secs(5);
            while !fs::try_exists(&child_marker).await.unwrap_or(false)
                && time::Instant::now() < deadline
            {
                time::sleep(Duration::from_millis(25)).await;
            }
            fs::write(cancel, "{}").await.unwrap();
        });
        let runner = CodexRunner::new(
            worker.to_string_lossy(),
            Duration::from_secs(15),
            temp.path().join("codex-home"),
            PathBuf::from("/tmp/kurbis-ai-controller"),
        );
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
