//! `multi git ...`: run one git command across the root and every sub-repo.

use std::path::Path;

use crate::error::{Error, Result};
use crate::git::{check_all_on_same_branch, git_output, stdout_string};
use crate::paths::Paths;
use crate::repos::load_repos;

pub fn run_git_command(repo_path: &Path, git_args: &[String]) -> Result<()> {
    let joined = git_args.join(" ");
    log::info!("Running 'git {joined}' in {}", repo_path.display());
    let args: Vec<&str> = git_args.iter().map(String::as_str).collect();
    let output = git_output(repo_path, &args)?;
    if !output.status.success() {
        log::error!("Failed to run git command in {}", repo_path.display());
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        };
        return Err(Error::Git(format!(
            "Failed to run git command in {}{detail}",
            repo_path.display()
        )));
    }
    log::info!(
        "Output from {}:\n{}",
        repo_path.display(),
        stdout_string(&output).trim()
    );
    Ok(())
}

/// Run `git_args` in the root first, then in each cloned sub-repo. Requires
/// every cloned repo to be on its expected branch (dirty trees are fine).
pub fn run_git_in_all_repos(paths: &Paths, git_args: &[String]) -> Result<()> {
    if paths.settings()?.is_monorepo() {
        return Err(Error::Usage(
            "The 'multi git' command is not available in monorepo mode. Use git directly in the root workspace."
                .to_string(),
        ));
    }
    check_all_on_same_branch(paths, true)?;
    run_git_command(&paths.root_dir, git_args)?;
    for repo in load_repos(paths)? {
        if !repo.is_cloned() {
            log::info!("Skipping {}: not cloned in this workspace", repo.name);
            continue;
        }
        run_git_command(&repo.path, git_args)?;
    }
    Ok(())
}
