use std::path::{Path, PathBuf};

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

    pub fn bridge_data(&self) -> PathBuf {
        self.server().join("plugins/data/vibe-bridge")
    }

    pub fn requests(&self) -> PathBuf {
        self.bridge_data().join("requests")
    }

    pub fn processing(&self) -> PathBuf {
        self.bridge_data().join("processing")
    }

    pub fn results(&self) -> PathBuf {
        self.bridge_data().join("results")
    }

    pub fn cancel(&self) -> PathBuf {
        self.bridge_data().join("cancel")
    }

    pub fn plugin_index(&self) -> PathBuf {
        self.bridge_data().join("plugins.json")
    }

    pub fn workspace_plugins(&self) -> PathBuf {
        self.root.join("workspace/plugins")
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
            self.workspace_plugins(),
            self.installed_plugins(),
        ] {
            tokio::fs::create_dir_all(dir).await?;
        }
        Ok(())
    }
}
