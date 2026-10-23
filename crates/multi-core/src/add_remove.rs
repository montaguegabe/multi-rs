//! `multi add` and `multi remove`.

use std::path::Path;

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::fsx;
use crate::git::{check_repo_is_clean, run_git};
use crate::ignore_files::{
    clear_subrepo_generated_file_block, remove_gitignore_entries_for_repos,
    remove_ignore_entries_for_repos,
};
use crate::jsonc::{read_json_object, write_json_object_2};
use crate::paths::Paths;
use crate::repo_urls::{derive_explicit_local_name, derive_repo_slug_from_url, normalize_repo_url};
use crate::sync::vscode::merge_vscode_configs;
use crate::sync::{sync, SyncOptions};

fn entry_name(entry: &Value) -> String {
    entry
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            derive_repo_slug_from_url(entry.get("url").and_then(Value::as_str).unwrap_or(""))
        })
}

fn repos_list(config: &Map<String, Value>) -> Vec<Value> {
    config
        .get("repos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Append `repo_url` to multi.json and sync. Returns the local repo name.
pub fn add_repo(target_dir: &Path, repo_url: &str, name: Option<&str>) -> Result<String> {
    let paths = Paths::discover(target_dir, None)?;
    let is_monorepo = paths.settings()?.is_monorepo();
    let mut config = read_json_object(&paths.multi_json_path())?;
    let mut repos = repos_list(&config);

    let normalized_new = normalize_repo_url(repo_url);
    for entry in &repos {
        let existing = entry.get("url").and_then(Value::as_str).unwrap_or("");
        if normalize_repo_url(existing) == normalized_new {
            return Err(Error::Other(format!(
                "A repo with URL '{existing}' already exists in multi.json."
            )));
        }
    }

    let mut auto_name = derive_explicit_local_name(repo_url, &paths.root_name());
    let slug_name = derive_repo_slug_from_url(repo_url);
    let existing_names: std::collections::HashSet<String> = repos.iter().map(entry_name).collect();

    if name.is_none() {
        if let Some(candidate) = &auto_name {
            if existing_names.contains(candidate) || paths.root_dir.join(candidate).exists() {
                auto_name = None;
            }
        }
    }
    let repo_name = name
        .map(str::to_string)
        .or_else(|| auto_name.clone())
        .unwrap_or(slug_name);

    if existing_names.contains(&repo_name) {
        return Err(Error::Other(format!(
            "A repo with name '{repo_name}' already exists in multi.json."
        )));
    }
    let target = paths.root_dir.join(&repo_name);
    if target.exists() {
        return Err(Error::Other(format!(
            "Directory '{repo_name}' already exists on disk."
        )));
    }

    // In monorepo mode, clone and remove .git before writing multi.json,
    // since sync skips cloning in monorepo mode.
    if is_monorepo {
        log::info!("Cloning {repo_name}...");
        let target_str = target.display().to_string();
        run_git(&paths.root_dir, &["clone", "-q", repo_url, &target_str])?;
        let git_dir = target.join(".git");
        if git_dir.exists() || fsx::is_symlink(&git_dir) {
            fsx::remove_path(&git_dir)?;
        }
        log::info!("Cloned {repo_name} and removed .git for monorepo mode");
    }

    let mut entry = Map::new();
    entry.insert("url".to_string(), Value::String(repo_url.to_string()));
    if let Some(name) = name {
        entry.insert("name".to_string(), Value::String(name.to_string()));
    } else if let Some(auto) = auto_name {
        entry.insert("name".to_string(), Value::String(auto));
    }
    repos.push(Value::Object(entry));
    config.insert("repos".to_string(), Value::Array(repos));
    write_json_object_2(&paths.multi_json_path(), &config)?;
    log::info!("Added '{repo_name}' to multi.json");

    sync(&paths.root_dir, &SyncOptions::default())?;
    log::info!("✅ Successfully added {repo_name}");
    Ok(repo_name)
}

/// Options for [`remove_repo`].
#[derive(Debug, Clone, Default)]
pub struct RemoveOptions {
    /// Also delete the local directory or symlink.
    pub delete: bool,
    /// Skip the uncommitted-changes check before deleting.
    pub force: bool,
}

/// Remove `repo_name` from multi.json and clean up ignore files.
pub fn remove_repo(target_dir: &Path, repo_name: &str, options: &RemoveOptions) -> Result<()> {
    let paths = Paths::discover(target_dir, None)?;
    let is_monorepo = paths.settings()?.is_monorepo();
    let mut config = read_json_object(&paths.multi_json_path())?;
    let mut repos = repos_list(&config);

    let index = repos
        .iter()
        .position(|entry| entry_name(entry) == repo_name)
        .ok_or_else(|| Error::Other(format!("No repo named '{repo_name}' found in multi.json.")))?;

    let relative_path = repos[index]
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or(repo_name)
        .trim_matches('/')
        .to_string();
    let repo_path = paths.root_dir.join(&relative_path);

    if options.delete && (repo_path.exists() || fsx::is_symlink(&repo_path)) {
        if !options.force && !is_monorepo {
            if fsx::is_symlink(&repo_path) {
                log::debug!("{repo_name} is a symlink, skipping clean check");
            } else {
                check_repo_is_clean(&repo_path, true)?;
            }
        }
        if fsx::is_symlink(&repo_path) {
            fsx::remove_file(&repo_path)?;
            log::info!("Removed symlink '{repo_name}'");
        } else {
            fsx::remove_dir_all(&repo_path)?;
            log::info!("Deleted directory '{repo_name}'");
        }
    }

    repos.remove(index);
    config.insert("repos".to_string(), Value::Array(repos));
    write_json_object_2(&paths.multi_json_path(), &config)?;
    log::info!("Removed '{repo_name}' from multi.json");

    if !is_monorepo {
        remove_gitignore_entries_for_repos(&paths, std::slice::from_ref(&relative_path))?;
        remove_ignore_entries_for_repos(&paths, std::slice::from_ref(&relative_path))?;
        if repo_path.exists() {
            clear_subrepo_generated_file_block(&repo_path)?;
        }
    }

    merge_vscode_configs(&paths)?;
    log::info!("✅ Successfully removed {repo_name}");
    Ok(())
}
