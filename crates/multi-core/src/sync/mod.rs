//! `multi sync`: clone/symlink repos, install hooks, merge VS Code config,
//! generate agent files and monorepo GitHub workflows.

pub mod agents;
pub mod github;
pub mod vscode;

use std::path::Path;

use crate::bootstrap::{ensure_root_git_repo, ensure_workspace_readme};
use crate::doctor::find_nested_git_repos_in_monorepo;
use crate::error::Result;
use crate::fsx;
use crate::git::{
    expected_branch_for_repo, get_current_branch, git_output, run_git, stderr_string,
};
use crate::hooks::install_hooks;
use crate::ignore_files::{
    update_gitignore_with_generated_files, update_gitignore_with_repos, update_ignore_with_repos,
};
use crate::paths::Paths;
use crate::registry::{lookup_repo, register_repo};
use crate::repos::{load_repos, Repository};
use crate::settings::Settings;

fn try_symlink_repo(
    repo: &Repository,
    paths: &Paths,
    settings: &Settings,
    expected_branch: Option<&str>,
) -> Result<bool> {
    if !settings.allow_symlinks() || !repo.allow_symlink {
        return Ok(false);
    }
    let Some(url) = &repo.url else {
        return Ok(false);
    };
    let Some(existing_path) = lookup_repo(url) else {
        return Ok(false);
    };

    // Avoid recursive symlinks: never link to a clone inside this workspace.
    if fsx::is_inside(&existing_path, &paths.root_dir) {
        log::debug!(
            "Existing repo at {} is inside current workspace, will clone instead",
            existing_path.display()
        );
        return Ok(false);
    }

    // Virtual environments contain absolute paths that break with symlinks.
    for venv_name in ["venv", ".venv"] {
        if existing_path.join(venv_name).exists() {
            log::warn!(
                "⚠️  Cannot symlink {}: source repo contains {venv_name}/, will clone instead",
                repo.name
            );
            return Ok(false);
        }
    }

    if let Some(parent) = repo.path.parent() {
        fsx::create_dir_all(parent)?;
    }
    if let Err(err) = fsx::symlink(&existing_path, &repo.path) {
        log::warn!("Failed to create symlink for {}: {err}", repo.name);
        return Ok(false);
    }
    log::info!("🔗 Symlinked {} -> {}", repo.name, existing_path.display());

    if let Some(branch) = expected_branch {
        match run_git(&repo.path, &["checkout", branch]) {
            Ok(_) => log::debug!("Checked out branch {branch} in symlinked repo"),
            Err(_) => log::warn!(
                "Branch {branch} not found in {}, staying on current branch.",
                repo.name
            ),
        }
    }
    Ok(true)
}

fn clone_into(url: &str, destination: &Path, branch: Option<&str>) -> Result<std::process::Output> {
    let destination_str = destination.display().to_string();
    let mut args = vec!["clone", "-q"];
    if let Some(branch) = branch {
        args.extend(["--branch", branch, "--single-branch"]);
    }
    args.extend([url, destination_str.as_str()]);
    let parent = destination.parent().unwrap_or(Path::new("."));
    git_output(parent, &args)
}

fn clone_repo(repo: &Repository, expected_branch: Option<&str>) -> Result<()> {
    log::debug!("Cloning {}...", repo.name);
    let url = repo.url.as_deref().ok_or_else(|| {
        crate::error::Error::Config(format!("Repository {} has no url to clone.", repo.name))
    })?;

    // Clone outside the workspace first, then move the checked-out repo into
    // place. This avoids checkout conflicts when the workspace root gitignore
    // already manages sub-repo paths.
    let temp = tempfile::Builder::new()
        .prefix(&format!("multi-{}-clone-", repo.name))
        .tempdir()?;
    let temp_repo_path = temp.path().join(&repo.name);

    let mut output = clone_into(url, &temp_repo_path, expected_branch)?;
    if !output.status.success() {
        if expected_branch.is_none() {
            return Err(crate::error::Error::Git(format!(
                "Failed to clone {}: {}",
                url,
                stderr_string(&output).trim()
            )));
        }
        // Preserve the historical fallback for repos that do not have the
        // mirrored branch: clone the remote default, then warn below when
        // checkout cannot find the expected branch.
        if temp_repo_path.exists() {
            fsx::remove_dir_all(&temp_repo_path)?;
        }
        output = clone_into(url, &temp_repo_path, None)?;
        if !output.status.success() {
            return Err(crate::error::Error::Git(format!(
                "Failed to clone {}: {}",
                url,
                stderr_string(&output).trim()
            )));
        }
    }

    if let Some(branch) = expected_branch {
        match run_git(&temp_repo_path, &["checkout", "-q", branch]) {
            Ok(_) => log::info!("✅ Cloned {} and checked out branch {branch}", repo.name),
            Err(_) => log::warn!(
                "Branch {branch} not found in {}, staying on default branch.",
                repo.name
            ),
        }
    }

    fsx::move_path(&temp_repo_path, &repo.path)
}

/// Clone (or symlink) every repository that is missing on disk and refresh
/// the managed `.gitignore` / `.ignore` entries.
pub fn clone_repos(paths: &Paths, ensure_on_same_branch: bool) -> Result<()> {
    let settings = paths.settings()?;
    let repos = load_repos(paths)?;

    let current_branch = if ensure_on_same_branch {
        let branch = get_current_branch(&paths.root_dir)?;
        log::info!("Current branch: {branch}");
        Some(branch)
    } else {
        None
    };

    for repo in &repos {
        let expected_branch = expected_branch_for_repo(repo, current_branch.as_deref());
        if repo.path.exists() {
            log::debug!("{} already exists, registering and skipping...", repo.name);
            if let Some(url) = &repo.url {
                register_repo(url, &repo.path);
            }
            continue;
        }
        if try_symlink_repo(repo, paths, &settings, expected_branch.as_deref())? {
            continue;
        }
        clone_repo(repo, expected_branch.as_deref())?;
        if let Some(url) = &repo.url {
            register_repo(url, &repo.path);
        }
    }

    update_gitignore_with_repos(paths)?;
    update_ignore_with_repos(paths)?;
    Ok(())
}

/// Options for a full sync.
#[derive(Debug, Clone)]
pub struct SyncOptions {
    /// Mirror the root branch into freshly cloned sub-repos.
    pub ensure_on_same_branch: bool,
    pub install_set: Option<String>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            ensure_on_same_branch: true,
            install_set: None,
        }
    }
}

/// Run all sync operations for the workspace at `root_dir`.
pub fn sync(root_dir: &Path, options: &SyncOptions) -> Result<()> {
    log::info!("Syncing...");
    let paths = Paths::discover(root_dir, options.install_set.as_deref())?;
    ensure_root_git_repo(&paths.root_dir)?;
    ensure_workspace_readme(&paths)?;

    let nested = find_nested_git_repos_in_monorepo(&paths)?;
    if !nested.is_empty() {
        log::warn!(
            "monoRepo mode expects listed directories to be part of the root git repo (no nested .git). Found nested git repos: {}.",
            nested.join(", ")
        );
    }

    if !paths.settings()?.is_monorepo() {
        clone_repos(&paths, options.ensure_on_same_branch)?;
    }

    install_hooks(&paths)?;
    update_gitignore_with_generated_files(&paths)?;
    vscode::merge_vscode_configs(&paths)?;
    agents::sync_all_agents(&paths)?;
    github::sync_all_github_actions(&paths)?;

    log::info!("✅ Sync complete");
    Ok(())
}
