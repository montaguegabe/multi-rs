//! `multi convert-monorepo`: turn a standard workspace into monorepo mode.

use std::path::Path;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::fsx;
use crate::ignore_files::{remove_gitignore_entries_for_repos, remove_ignore_entries_for_repos};
use crate::jsonc::{read_json_object, write_json_object_2};
use crate::paths::Paths;
use crate::repos::load_repos;

fn remove_nested_git_dirs(paths: &Paths) -> Result<()> {
    for repo in load_repos(paths)? {
        let git_path = repo.path.join(".git");
        if git_path.is_dir() && !fsx::is_symlink(&git_path) {
            fsx::remove_dir_all(&git_path)?;
            log::info!("✅ Removed nested git directory: {}/.git", repo.name);
        } else if git_path.exists() || fsx::is_symlink(&git_path) {
            fsx::remove_file(&git_path)?;
            log::info!("✅ Removed nested git file: {}/.git", repo.name);
        } else {
            log::warn!("No nested .git found for {}; skipping", repo.name);
        }
    }
    Ok(())
}

fn set_monorepo_flag(paths: &Paths) -> Result<()> {
    let mut config = read_json_object(&paths.multi_json_path())?;
    config.insert("monoRepo".to_string(), Value::Bool(true));
    write_json_object_2(&paths.multi_json_path(), &config)
}

/// Delete nested `.git` directories, set `monoRepo: true`, and drop the
/// managed ignore entries. Destructive; callers must confirm first.
pub fn convert_to_monorepo(root_dir: &Path) -> Result<()> {
    let paths = Paths::discover(root_dir, None)?;
    if paths.settings()?.is_monorepo() {
        return Err(Error::Usage(
            "Workspace is already in monorepo mode.".to_string(),
        ));
    }
    let repo_names: Vec<String> = load_repos(&paths)?.into_iter().map(|r| r.name).collect();
    remove_nested_git_dirs(&paths)?;
    set_monorepo_flag(&paths)?;
    remove_gitignore_entries_for_repos(&paths, &repo_names)?;
    remove_ignore_entries_for_repos(&paths, &repo_names)?;
    log::info!("✅ Converted workspace to monorepo mode");
    Ok(())
}
