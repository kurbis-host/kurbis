use std::{
    io,
    path::{Component, Path, PathBuf},
};

fn reject_symlink_components(path: &Path, boundary: &Path) -> anyhow::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => current.push(".."),
            Component::Normal(part) => current.push(part),
        }
        if !current.starts_with(boundary) {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "runtime path contains a symlink: {}",
                current.display()
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct RuntimePaths {
    root: PathBuf,
}

impl RuntimePaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn server(&self) -> PathBuf {
        self.root.join("server")
    }

    pub fn plugin_data(&self) -> PathBuf {
        self.server().join("plugins/data/kurbis-ai")
    }

    pub fn requests(&self) -> PathBuf {
        self.plugin_data().join("requests")
    }

    pub fn processing(&self) -> PathBuf {
        self.plugin_data().join("processing")
    }

    pub fn results(&self) -> PathBuf {
        self.plugin_data().join("results")
    }

    pub fn cancel(&self) -> PathBuf {
        self.plugin_data().join("cancel")
    }

    pub fn mcp(&self) -> PathBuf {
        self.plugin_data().join("mcp")
    }

    pub fn mcp_jobs(&self) -> PathBuf {
        self.mcp().join("jobs")
    }

    pub fn mcp_requests(&self) -> PathBuf {
        self.mcp().join("requests")
    }

    pub fn mcp_results(&self) -> PathBuf {
        self.mcp().join("results")
    }

    pub fn mcp_audit(&self) -> PathBuf {
        self.mcp().join("audit")
    }

    pub fn codex_home(&self) -> PathBuf {
        self.root.join("codex-home")
    }

    pub fn operator_authorizations(&self) -> PathBuf {
        self.plugin_data().join("authorization/operators.json")
    }

    pub fn plugin_index(&self) -> PathBuf {
        self.plugin_data().join("plugins.json")
    }

    pub fn workspace_plugins(&self) -> PathBuf {
        self.root.join("workspace/plugins")
    }

    pub fn git_repositories(&self) -> PathBuf {
        self.root.join("workspace/.git-repositories")
    }

    pub fn git_repository(&self, plugin: &str) -> PathBuf {
        self.git_repositories().join(format!("{plugin}.git"))
    }

    pub fn codex_sessions(&self) -> PathBuf {
        self.root.join("codex-sessions")
    }

    pub fn codex_session_home(&self, session: uuid::Uuid) -> PathBuf {
        self.codex_sessions().join(session.to_string())
    }

    pub fn plugin_workspace(&self, plugin: &str) -> PathBuf {
        self.workspace_plugins().join(plugin)
    }

    pub fn archived_artifacts(&self, plugin: &str) -> PathBuf {
        self.root.join("workspace/artifacts").join(plugin)
    }

    pub fn installed_plugins(&self) -> PathBuf {
        self.server().join("plugins")
    }

    pub fn installed_plugin(&self, plugin: &str) -> PathBuf {
        self.installed_plugins().join(format!("{plugin}.wasm"))
    }

    pub async fn create_dirs(&self) -> anyhow::Result<()> {
        for dir in [
            self.requests(),
            self.processing(),
            self.results(),
            self.cancel(),
            self.mcp_jobs(),
            self.mcp_requests(),
            self.mcp_results(),
            self.mcp_audit(),
            self.codex_home(),
            self.workspace_plugins(),
            self.git_repositories(),
            self.codex_sessions(),
            self.installed_plugins(),
        ] {
            reject_symlink_components(&dir, &self.root)?;
            tokio::fs::create_dir_all(&dir).await?;
            reject_symlink_components(&dir, &self.root)?;
        }
        Ok(())
    }
}
