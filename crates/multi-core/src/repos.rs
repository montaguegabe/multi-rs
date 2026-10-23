//! Repository entries declared in `multi.json`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::paths::Paths;
use crate::settings::normalize_relative_posix;

/// One `repos[]` entry, with derived name and on-disk path.
#[derive(Debug, Clone)]
pub struct Repository {
    /// Git URL (optional in monorepo mode).
    pub url: Option<String>,
    /// Identity of the repo: explicit `name` or the last URL segment.
    pub name: String,
    /// Workspace-relative on-disk location (`path`, defaults to `name`).
    pub relative_path: String,
    /// Absolute on-disk location.
    pub path: PathBuf,
    pub description: Option<String>,
    pub skip_vscode: bool,
    pub allow_symlink: bool,
    pub manage_gitignore: bool,
    pub fixed_branch: Option<String>,
    pub install_sets: Option<Vec<String>>,
    pub required_tasks: Vec<String>,
    pub required_compounds: Vec<String>,
    pub required_launch_configurations: Vec<String>,
    /// Per-repo `vscode` config (for example `settingsOverrides`).
    pub vscode: Option<Map<String, Value>>,
    /// Any other keys from the repo config.
    pub extra: Map<String, Value>,
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

impl Repository {
    /// Build a repository from its raw `multi.json` entry.
    pub fn from_config(root_dir: &Path, config: &Value) -> Result<Self> {
        let Value::Object(map) = config else {
            return Err(Error::Config(
                "Each repository config in multi.json must be an object.".to_string(),
            ));
        };
        let mut extra = map.clone();

        let url = match extra.remove("url") {
            Some(Value::String(url)) => Some(url),
            Some(Value::Null) | None => None,
            Some(_) => {
                return Err(Error::Config(
                    "Repository 'url' in multi.json must be a string.".to_string(),
                ))
            }
        };
        if let Some(url) = &url {
            if url.ends_with(".git") {
                return Err(Error::Config(format!(
                    "Repository URL must not end with '.git': {url}. Update multi.json and rerun `multi sync`."
                )));
            }
        }

        let name = match extra.remove("name") {
            Some(Value::String(name)) => name,
            Some(other) if !other.is_null() => other.to_string(),
            _ => match &url {
                Some(url) => url.rsplit('/').next().unwrap_or_default().to_string(),
                None => {
                    return Err(Error::Config(
                        "Repository must have either 'url' or 'name'".to_string(),
                    ))
                }
            },
        };

        let relative_path = match extra.remove("path") {
            None | Some(Value::Null) => name.clone(),
            Some(Value::String(raw)) => normalize_relative_posix(&raw)
                .filter(|p| p != ".")
                .ok_or_else(|| {
                    Error::Config(format!(
                        "path for repository {name} must be a relative path inside the workspace (no leading '/' and no '..')."
                    ))
                })?,
            Some(_) => {
                return Err(Error::Config(format!(
                    "path for repository {name} must be a string."
                )))
            }
        };
        let path = root_dir.join(&relative_path);

        let skip_vscode = extra
            .remove("skipVSCode")
            .map(|v| crate::settings::truthy(&v))
            .unwrap_or(false);
        let allow_symlink = extra
            .remove("allowSymlink")
            .map(|v| crate::settings::truthy(&v))
            .unwrap_or(false);
        let manage_gitignore = extra
            .remove("manageGitignore")
            .map(|v| crate::settings::truthy(&v))
            .unwrap_or(true);

        let fixed_branch = match extra.remove("fixedBranch") {
            None | Some(Value::Null) => None,
            Some(Value::String(branch)) => Some(branch),
            Some(_) => {
                return Err(Error::Config(format!(
                    "fixedBranch for repository {name} must be a string."
                )))
            }
        };

        let install_sets = match extra.remove("installSets") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => {
                let mut sets = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Value::String(set) => sets.push(set),
                        _ => {
                            return Err(Error::Config(format!(
                                "installSets for repository {name} must be a list of strings."
                            )))
                        }
                    }
                }
                Some(sets)
            }
            Some(_) => {
                return Err(Error::Config(format!(
                    "installSets for repository {name} must be a list of strings."
                )))
            }
        };

        let description = match extra.remove("description") {
            Some(Value::String(description)) => Some(description),
            _ => None,
        };
        let required_tasks = string_list(extra.remove("requiredTasks").as_ref());
        let required_compounds = string_list(extra.remove("requiredCompounds").as_ref());
        let required_launch_configurations =
            string_list(extra.remove("requiredLaunchConfigurations").as_ref());
        let vscode = match extra.remove("vscode") {
            Some(Value::Object(map)) => Some(map),
            _ => None,
        };

        Ok(Repository {
            url,
            name,
            relative_path,
            path,
            description,
            skip_vscode,
            allow_symlink,
            manage_gitignore,
            fixed_branch,
            install_sets,
            required_tasks,
            required_compounds,
            required_launch_configurations,
            vscode,
            extra,
        })
    }

    /// Non-empty, trimmed description.
    pub fn description_text(&self) -> Option<&str> {
        self.description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
    }

    pub fn is_python(&self) -> bool {
        [
            "pyproject.toml",
            "requirements.txt",
            "Pipfile",
            "setup.py",
            "environment.yml",
            "setup.cfg",
        ]
        .iter()
        .any(|file| self.path.join(file).exists())
    }

    /// `false` only when the repo was never cloned into this workspace.
    ///
    /// A partial install set leaves those paths absent. A non-empty directory
    /// without git metadata still counts as present, so a damaged checkout
    /// keeps failing loudly instead of being skipped.
    pub fn is_cloned(&self) -> bool {
        if self.path.join(".git").exists() {
            return true;
        }
        self.path.is_dir()
            && std::fs::read_dir(&self.path)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false)
    }

    pub fn matches_install_set(&self, install_set: Option<&str>) -> bool {
        match (install_set, &self.install_sets) {
            (None, _) | (_, None) => true,
            (Some(set), Some(sets)) => sets.iter().any(|s| s == set),
        }
    }
}

/// Load the repositories declared in `multi.json`, filtered by the install
/// set selected on `paths`.
pub fn load_repos(paths: &Paths) -> Result<Vec<Repository>> {
    let settings = paths.settings()?;
    load_repos_from_settings(paths, &settings)
}

pub fn load_repos_from_settings(
    paths: &Paths,
    settings: &crate::settings::Settings,
) -> Result<Vec<Repository>> {
    let is_monorepo = settings.is_monorepo();
    let mut result = Vec::new();
    for config in settings.repo_configs() {
        let Value::Object(map) = &config else {
            return Err(Error::Config(
                "Each repository config in multi.json must be an object.".to_string(),
            ));
        };
        if !map.contains_key("url") && !map.contains_key("name") {
            return Err(Error::Config(
                "Repository config in multi.json must contain 'url' or 'name' field.".to_string(),
            ));
        }
        if !is_monorepo && !map.contains_key("url") {
            return Err(Error::Config(
                "Repository config must contain 'url' field (or enable monoRepo mode).".to_string(),
            ));
        }
        let repo = Repository::from_config(&paths.root_dir, &config)?;
        if repo.matches_install_set(paths.install_set.as_deref()) {
            result.push(repo);
        }
    }
    if result.is_empty() {
        return Err(match &paths.install_set {
            Some(set) => {
                Error::NoRepositories(format!("No repositories found for install set '{set}'."))
            }
            None => {
                Error::NoRepositories("No repositories found in multi.json settings.".to_string())
            }
        });
    }
    Ok(result)
}

/// Like [`load_repos`], minus repos that are not cloned in this workspace.
pub fn load_cloned_repos(paths: &Paths) -> Result<Vec<Repository>> {
    Ok(load_repos(paths)?
        .into_iter()
        .filter(Repository::is_cloned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derives_name_and_path() {
        let repo = Repository::from_config(
            Path::new("/ws"),
            &json!({"url": "https://github.com/org/api", "path": "services/api/"}),
        )
        .unwrap();
        assert_eq!(repo.name, "api");
        assert_eq!(repo.relative_path, "services/api");
        assert_eq!(repo.path, PathBuf::from("/ws/services/api"));
        assert!(repo.manage_gitignore);
    }

    #[test]
    fn rejects_bad_config() {
        assert!(
            Repository::from_config(Path::new("/ws"), &json!({"url": "https://x/y.git"})).is_err()
        );
        assert!(Repository::from_config(
            Path::new("/ws"),
            &json!({"url": "https://x/y", "path": "/abs"})
        )
        .is_err());
        assert!(Repository::from_config(
            Path::new("/ws"),
            &json!({"url": "https://x/y", "path": "../up"})
        )
        .is_err());
        assert!(Repository::from_config(
            Path::new("/ws"),
            &json!({"url": "https://x/y", "fixedBranch": 3})
        )
        .is_err());
        assert!(Repository::from_config(
            Path::new("/ws"),
            &json!({"url": "https://x/y", "installSets": "dev"})
        )
        .is_err());
        assert!(Repository::from_config(Path::new("/ws"), &json!({})).is_err());
    }

    #[test]
    fn install_sets() {
        let repo = Repository::from_config(
            Path::new("/ws"),
            &json!({"url": "https://x/y", "installSets": ["dev"]}),
        )
        .unwrap();
        assert!(repo.matches_install_set(None));
        assert!(repo.matches_install_set(Some("dev")));
        assert!(!repo.matches_install_set(Some("default")));
        let legacy =
            Repository::from_config(Path::new("/ws"), &json!({"url": "https://x/y"})).unwrap();
        assert!(legacy.matches_install_set(Some("default")));
    }
}
