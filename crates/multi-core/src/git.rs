//! Git helpers shared by every command. All git access shells out to the
//! `git` binary so hooks, credentials and worktrees behave exactly as they do
//! on the command line.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::fsx;
use crate::paths::Paths;
use crate::repos::{load_cloned_repos, load_repos, Repository};

/// Run git with `args` in `cwd`, capturing output. Only fails when git could
/// not be started.
pub fn git_output(cwd: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Git(format!("Failed to run git {}: {e}", args.join(" "))))
}

/// Run git with a timeout; returns `None` when the timeout elapsed.
pub fn git_output_timeout(cwd: &Path, args: &[&str], timeout: Duration) -> Result<Option<Output>> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Git(format!("Failed to run git {}: {e}", args.join(" "))))?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map(Some)
                    .map_err(|e| Error::Git(e.to_string()))
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(Error::Git(e.to_string())),
        }
    }
}

pub fn stdout_string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr_string(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Run git and return trimmed stdout, or a [`Error::Git`] carrying stderr.
pub fn run_git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(cwd, args)?;
    if output.status.success() {
        Ok(stdout_string(&output).trim().to_string())
    } else {
        let stderr = stderr_string(&output);
        let detail = if stderr.trim().is_empty() {
            String::new()
        } else {
            format!(": {}", stderr.trim())
        };
        Err(Error::Git(format!(
            "Failed to run git {} in {}{detail}",
            args.join(" "),
            cwd.display()
        )))
    }
}

/// Run git feeding `input` on stdin.
pub fn run_git_with_input(cwd: &Path, args: &[&str], input: &str) -> Result<String> {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Git(format!("Failed to run git {}: {e}", args.join(" "))))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| Error::Git(e.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| Error::Git(e.to_string()))?;
    if output.status.success() {
        Ok(stdout_string(&output))
    } else {
        Err(Error::Git(format!(
            "git {} failed (code {}): {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            stderr_string(&output).trim()
        )))
    }
}

/// Branch a sub-repo should be on: `fixedBranch` wins, otherwise the
/// workspace branch. This is the single fixed-branch invariant.
pub fn expected_branch_for_repo(
    repo: &Repository,
    workspace_branch: Option<&str>,
) -> Option<String> {
    repo.fixed_branch
        .clone()
        .or_else(|| workspace_branch.map(str::to_string))
}

pub fn repo_uses_fixed_branch(repo: &Repository) -> bool {
    repo.fixed_branch.is_some()
}

pub fn expected_branch_description(repo: &Repository, workspace_branch: Option<&str>) -> String {
    match &repo.fixed_branch {
        Some(fixed) => format!("fixed branch {fixed}"),
        None => format!("root branch {}", workspace_branch.unwrap_or("HEAD")),
    }
}

/// Top-level working tree of the repository containing `path`, if any.
pub fn git_top_level(path: &Path) -> Option<PathBuf> {
    let output = git_output(path, &["rev-parse", "--show-toplevel"]).ok()?;
    if !output.status.success() {
        return None;
    }
    let top = stdout_string(&output).trim().to_string();
    if top.is_empty() {
        return None;
    }
    Some(fsx::resolve(Path::new(&top)))
}

/// `true` when `repo_path` is the root of a working tree (main or linked
/// worktree). A directory that merely sits inside another repo is not.
pub fn is_git_repo_root(repo_path: &Path) -> bool {
    if !repo_path.join(".git").exists() {
        return false;
    }
    match git_top_level(repo_path) {
        Some(top) => top == fsx::resolve(repo_path),
        None => false,
    }
}

fn require_git_repo(repo_path: &Path) -> Result<()> {
    if repo_path.join(".git").exists() {
        return Ok(());
    }
    Err(Error::Git(format!(
        "Could not determine current branch for {}: not a git repository. Run `multi sync` from the workspace root.",
        fsx::file_name(repo_path)
    )))
}

fn symbolic_branch(repo_path: &Path) -> Result<Option<String>> {
    let output = git_output(repo_path, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    if output.status.success() {
        let branch = stdout_string(&output).trim().to_string();
        if !branch.is_empty() {
            return Ok(Some(branch));
        }
    }
    // Exit status 1 means detached HEAD; anything else is a broken repo.
    if output.status.code() == Some(1) || output.status.success() {
        return Ok(None);
    }
    Err(Error::Git(format!(
        "Could not determine current branch for {}: {}",
        fsx::file_name(repo_path),
        stderr_string(&output).trim()
    )))
}

/// Current branch name, or `HEAD` when detached.
pub fn get_current_branch(repo_path: &Path) -> Result<String> {
    require_git_repo(repo_path)?;
    Ok(symbolic_branch(repo_path)?.unwrap_or_else(|| "HEAD".to_string()))
}

/// Current branch name, or `(detached at <short-sha>)`. Read-only and safe
/// with dirty trees and linked worktrees.
pub fn describe_head(repo_path: &Path) -> Result<String> {
    require_git_repo(repo_path)?;
    if let Some(branch) = symbolic_branch(repo_path)? {
        return Ok(branch);
    }
    let short = run_git(repo_path, &["rev-parse", "--short", "HEAD"])?;
    Ok(format!("(detached at {short})"))
}

/// Validate that every cloned sub-repo is on its expected branch.
///
/// Read-only: dirty trees are fine. Repos not cloned into this workspace are
/// skipped. Detached HEADs never match an expected branch name.
pub fn check_all_on_same_branch(paths: &Paths, raise_error: bool) -> Result<bool> {
    let root_branch = describe_head(&paths.root_dir)?;
    for repo in load_cloned_repos(paths)? {
        let branch = describe_head(&repo.path)?;
        let expected = expected_branch_for_repo(&repo, Some(&root_branch));
        if Some(&branch) != expected.as_ref() {
            if raise_error {
                let expectation = expected_branch_description(&repo, Some(&root_branch));
                return Err(Error::Git(format!(
                    "Repository {} is not on the expected branch. Please fix. {}: {branch}, Expected: {expectation}",
                    repo.name, repo.name
                )));
            }
            return Ok(false);
        }
    }
    Ok(true)
}

/// `git status --porcelain` is empty (untracked files count as dirty).
pub fn check_repo_is_clean(repo_path: &Path, raise_error: bool) -> Result<bool> {
    if !is_git_repo_root(repo_path) {
        return Err(Error::Git(format!(
            "{} is not a git repository or has not been initialized properly (no .git folder)",
            repo_path.display()
        )));
    }
    let output = git_output(repo_path, &["status", "--porcelain"])?;
    if !output.status.success() {
        return Err(Error::Git(
            "Failed to check working directory status".to_string(),
        ));
    }
    let is_clean = stdout_string(&output).trim().is_empty();
    if !is_clean {
        if raise_error {
            return Err(Error::RepoNotClean(format!(
                "Working directory is not clean in {}. Please commit or stash changes first.",
                repo_path.display()
            )));
        }
        return Ok(false);
    }
    Ok(true)
}

pub fn check_all_repos_are_clean(paths: &Paths, raise_error: bool) -> Result<bool> {
    if !check_repo_is_clean(&paths.root_dir, raise_error)? {
        return Ok(false);
    }
    for repo in load_repos(paths)? {
        if !check_repo_is_clean(&repo.path, raise_error)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn ref_exists(repo_path: &Path, reference: &str) -> Result<bool> {
    let output = git_output(repo_path, &["show-ref", "--verify", "--quiet", reference])?;
    Ok(output.status.success())
}

/// `(exists_locally, exists_on_origin)` for `branch_name`.
pub fn check_branch_existence(repo_path: &Path, branch_name: &str) -> Result<(bool, bool)> {
    if !repo_path.join(".git").exists() {
        return Err(Error::Git("Failed to check if branch exists".to_string()));
    }
    let local = ref_exists(repo_path, &format!("refs/heads/{branch_name}"))?;
    let remote = ref_exists(repo_path, &format!("refs/remotes/origin/{branch_name}"))?;
    Ok((local, remote))
}

/// `git config --get key`, `None` when unset.
pub fn git_config_get(repo_path: &Path, key: &str) -> Result<Option<String>> {
    let output = git_output(repo_path, &["config", "--get", key])?;
    if output.status.success() {
        return Ok(Some(stdout_string(&output).trim().to_string()));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(Error::Git(format!(
        "Could not read {key} in {}: {}",
        repo_path.display(),
        stderr_string(&output).trim()
    )))
}

pub fn git_config(repo_path: &Path, args: &[&str]) -> Result<()> {
    let mut full = vec!["config"];
    full.extend_from_slice(args);
    let output = git_output(repo_path, &full)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Git(format!(
            "Failed to run git config {} in {}: {}",
            args.join(" "),
            repo_path.display(),
            stderr_string(&output).trim()
        )))
    }
}

/// `git init -b main` when the directory is not already a repo root.
pub fn init_repo(path: &Path) -> Result<()> {
    fsx::create_dir_all(path)?;
    run_git(path, &["init", "-q", "-b", "main"]).map(|_| ())
}

/// Create `branch_name` if missing (locally or on origin) and switch to it.
pub fn create_and_switch_branch(
    repo_path: &Path,
    branch_name: &str,
    allow_create: bool,
) -> Result<()> {
    let (exists_locally, exists_remotely) = check_branch_existence(repo_path, branch_name)?;
    if exists_locally || exists_remotely {
        log::info!(
            "Branch '{branch_name}' already exists in {}",
            repo_path.display()
        );
        run_git(repo_path, &["checkout", branch_name])?;
    } else {
        if !allow_create {
            return Err(Error::Git(format!(
                "Branch '{branch_name}' does not exist in {}.  Normally we would create a new branch, but you started with different repos checked out to different branches, so there is no base branch to create from.",
                repo_path.display()
            )));
        }
        run_git(repo_path, &["checkout", "-b", branch_name])?;
    }
    log::info!(
        "✅ Switched to branch '{branch_name}' in {}",
        repo_path.display()
    );
    Ok(())
}

/// Commit everything in `repo_path` with `message`.
pub fn commit_all(repo_path: &Path, message: &str) -> Result<()> {
    run_git(repo_path, &["add", "-A"])?;
    run_git(repo_path, &["commit", "-q", "-m", message])?;
    Ok(())
}
