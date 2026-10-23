//! Workspace discovery and well-known paths.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::settings::Settings;

/// Locations inside a multi workspace. Created by walking up from a target
/// directory until a `multi.json` is found.
#[derive(Debug, Clone)]
pub struct Paths {
    pub target_dir: PathBuf,
    pub root_dir: PathBuf,
    pub install_set: Option<String>,
}

impl Paths {
    /// Discover the workspace root from `target_dir` (or any parent).
    pub fn discover(target_dir: impl AsRef<Path>, install_set: Option<&str>) -> Result<Self> {
        let target_dir = target_dir.as_ref().to_path_buf();
        let root_dir = find_root(&target_dir)?;
        Ok(Paths {
            target_dir,
            root_dir,
            install_set: install_set.map(str::to_string),
        })
    }

    /// Discover from the current working directory.
    pub fn from_cwd(install_set: Option<&str>) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        Self::discover(cwd, install_set)
    }

    /// Use `root_dir` as the workspace root without searching.
    pub fn at_root(root_dir: impl AsRef<Path>, install_set: Option<&str>) -> Self {
        let root_dir = root_dir.as_ref().to_path_buf();
        Paths {
            target_dir: root_dir.clone(),
            root_dir,
            install_set: install_set.map(str::to_string),
        }
    }

    pub fn with_install_set(mut self, install_set: Option<&str>) -> Self {
        self.install_set = install_set.map(str::to_string);
        self
    }

    pub fn settings(&self) -> Result<Settings> {
        Settings::from_multi_json_file(&self.multi_json_path())
    }

    pub fn root_name(&self) -> String {
        self.root_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root_dir.display().to_string())
    }

    pub fn multi_json_path(&self) -> PathBuf {
        self.root_dir.join("multi.json")
    }

    pub fn gitignore_path(&self) -> PathBuf {
        self.root_dir.join(".gitignore")
    }

    pub fn vscode_ignore_path(&self) -> PathBuf {
        self.root_dir.join(".ignore")
    }

    pub fn root_vscode_dir(&self) -> PathBuf {
        vscode_config_dir(&self.root_dir)
    }

    pub fn vscode_launch_path(&self) -> PathBuf {
        self.root_vscode_dir().join("launch.json")
    }

    pub fn vscode_tasks_path(&self) -> PathBuf {
        self.root_vscode_dir().join("tasks.json")
    }

    pub fn vscode_settings_path(&self) -> PathBuf {
        self.root_vscode_dir().join("settings.json")
    }

    pub fn vscode_settings_shared_path(&self) -> PathBuf {
        self.root_vscode_dir().join("settings.shared.json")
    }

    pub fn vscode_settings_local_path(&self) -> PathBuf {
        self.root_vscode_dir().join("settings.local.json")
    }

    pub fn vscode_launch_shared_path(&self) -> PathBuf {
        self.root_vscode_dir().join("launch.shared.json")
    }

    pub fn vscode_tasks_shared_path(&self) -> PathBuf {
        self.root_vscode_dir().join("tasks.shared.json")
    }

    pub fn vscode_extensions_path(&self) -> PathBuf {
        self.root_vscode_dir().join("extensions.json")
    }
}

pub fn vscode_config_dir(repo_dir: &Path) -> PathBuf {
    repo_dir.join(".vscode")
}

fn find_root(start_dir: &Path) -> Result<PathBuf> {
    let mut current = start_dir.to_path_buf();
    loop {
        if current.join("multi.json").exists() {
            return Ok(current);
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return Err(Error::NotAWorkspace),
        }
    }
}
