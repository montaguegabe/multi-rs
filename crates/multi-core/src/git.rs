//! Git helpers shared by every command. All git access shells out to the
//! `git` binary so hooks, credentials and worktrees behave exactly as they do
//! on the command line.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

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

/// Run git with a timeout; returns `None` when the timeout elapsed and the
/// process (with everything it spawned) was killed. Stdout and stderr are
/// drained concurrently, so output of any size cannot stall the command.
pub fn git_output_timeout(cwd: &Path, args: &[&str], timeout: Duration) -> Result<Option<Output>> {
    run_bounded(cwd, args, None, timeout)
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

/// Run git feeding `input` on stdin (for example a patch for `git apply`),
/// bounded by `timeout`. Returns stdout, or an [`Error::Git`] carrying stderr.
pub fn run_git_with_input(
    cwd: &Path,
    args: &[&str],
    input: &str,
    timeout: Duration,
) -> Result<String> {
    let Some(output) = run_bounded(cwd, args, Some(input), timeout)? else {
        return Err(Error::Git(format!(
            "git {} timed out after {}s",
            args.join(" "),
            timeout.as_secs()
        )));
    };
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

/// `true` when `repo_path` is the root of a working tree: it holds a `.git`
/// directory with a `HEAD`, or a `.git` file pointing at one (linked worktrees
/// and submodules). A directory that merely sits inside another repo is not.
/// This is a pure filesystem check so callers can afford it per repo.
pub fn is_git_repo_root(repo_path: &Path) -> bool {
    let dot_git = repo_path.join(".git");
    if dot_git.is_dir() {
        return dot_git.join("HEAD").is_file();
    }
    std::fs::read_to_string(&dot_git).is_ok_and(|text| text.starts_with("gitdir:"))
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

/// Spawn git in its own process group with piped stdio, feed it `input`, and
/// collect its output within `timeout`. Every pipe is served by its own
/// thread, so neither a large patch on stdin nor voluminous output can
/// deadlock the child. On timeout the whole process group is killed, which
/// also takes down hooks, ssh and credential helpers git started; the same
/// happens when git has exited but an orphan still holds its pipes open.
fn run_bounded(
    cwd: &Path,
    args: &[&str],
    input: Option<&str>,
    timeout: Duration,
) -> Result<Option<Output>> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| Error::Git(format!("Failed to run git {}: {e}", args.join(" "))))?;
    let pid = child.id();
    let deadline = Instant::now() + timeout;

    let stdin_writer = match (child.stdin.take(), input) {
        (Some(mut pipe), Some(text)) => {
            let text = text.to_owned();
            Some(thread::spawn(move || {
                match pipe.write_all(text.as_bytes()) {
                    // git may legitimately exit before reading all of its input
                    // (for example when the first hunk of a patch fails); its
                    // stderr, not EPIPE, then explains what happened.
                    Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e),
                    _ => Ok(()),
                }
            }))
        }
        _ => None,
    };
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let exit = wait_in_background(child);

    let status = match recv_until(&exit, deadline) {
        Some(status) => status,
        None => {
            kill_process_tree(pid);
            let _ = exit.recv();
            let _ = stdout.recv();
            let _ = stderr.recv();
            return Ok(None);
        }
    };
    let status = status
        .map_err(|e| Error::Git(format!("Failed to wait for git {}: {e}", args.join(" "))))?;
    if let Some(writer) = stdin_writer {
        writer
            .join()
            .map_err(|_| Error::Git("git stdin writer panicked".to_string()))?
            .map_err(|e| {
                Error::Git(format!(
                    "Failed to write input to git {}: {e}",
                    args.join(" ")
                ))
            })?;
    }
    let stdout = collect_until(&stdout, deadline, pid);
    let stderr = collect_until(&stderr, deadline, pid);
    Ok(Some(Output {
        status,
        stdout,
        stderr,
    }))
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let result = match pipe {
            Some(mut pipe) => pipe.read_to_end(&mut buffer).map(|_| buffer),
            None => Ok(buffer),
        };
        let _ = tx.send(result);
    });
    rx
}

fn wait_in_background(mut child: Child) -> Receiver<io::Result<std::process::ExitStatus>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    rx
}

/// Receive from `rx` before `deadline`; `None` when the deadline passed or the
/// sender went away without sending.
fn recv_until<T>(rx: &Receiver<T>, deadline: Instant) -> Option<T> {
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
}

/// Collect drained pipe bytes before `deadline`. If an orphaned descendant of
/// git still holds the pipe open past the deadline, kill the process group and
/// take whatever was read.
fn collect_until(rx: &Receiver<io::Result<Vec<u8>>>, deadline: Instant, pid: u32) -> Vec<u8> {
    let bytes = match recv_until(rx, deadline) {
        Some(bytes) => bytes,
        None => {
            kill_process_tree(pid);
            rx.recv().unwrap_or_else(|_| Ok(Vec::new()))
        }
    };
    bytes.unwrap_or_default()
}

/// Kill the process group led by `pid` (unix) or the process tree rooted at it
/// (Windows). Processes git spawned, like hooks and ssh, die with it.
fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        // SAFETY: killpg has no memory-safety preconditions; the pid came from
        // a child this module spawned with its own process group.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    fn scratch_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path()).unwrap();
        git_config(tmp.path(), &["user.name", "Tests"]).unwrap();
        git_config(tmp.path(), &["user.email", "tests@example.invalid"]).unwrap();
        git_config(tmp.path(), &["commit.gpgsign", "false"]).unwrap();
        tmp
    }

    #[test]
    fn timed_run_drains_output_larger_than_a_pipe_buffer() {
        let tmp = scratch_repo();
        let dir = tmp.path();
        // 3000 untracked files make `git status --porcelain` emit ~100KB, well
        // past the 64KB pipe buffer that used to deadlock the poll loop.
        for i in 0..3000 {
            std::fs::write(
                dir.join(format!("file-with-a-reasonably-long-name-{i:05}.txt")),
                "x",
            )
            .unwrap();
        }
        let output = git_output_timeout(dir, &["status", "--porcelain"], Duration::from_secs(30))
            .unwrap()
            .expect("status must not time out");
        assert!(output.status.success());
        assert!(
            output.stdout.len() > 65_536,
            "expected >64KB, got {}",
            output.stdout.len()
        );
        assert_eq!(stdout_string(&output).lines().count(), 3000);
    }

    #[test]
    fn timed_run_kills_hooks_and_reports_timeout() {
        let tmp = scratch_repo();
        let dir = tmp.path();
        git_config(dir, &["core.hooksPath", ".githooks"]).unwrap();
        std::fs::create_dir_all(dir.join(".githooks")).unwrap();
        let hook = dir.join(".githooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        run_git(dir, &["add", "a.txt"]).unwrap();
        let started = Instant::now();
        let result =
            git_output_timeout(dir, &["commit", "-m", "hang"], Duration::from_millis(500)).unwrap();
        assert!(result.is_none(), "a hanging hook must surface as a timeout");
        // Killing the process group also kills the hook's `sleep`, so this
        // returns promptly instead of waiting for the hook's pipes to close.
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn input_run_reports_git_stderr_when_patch_is_rejected() {
        let tmp = scratch_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        commit_all(dir, "init").unwrap();
        let bad_patch =
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-nope\n+two\n";
        let err = run_git_with_input(
            dir,
            &["apply", "--cached", "-"],
            bad_patch,
            Duration::from_secs(30),
        )
        .expect_err("patch must be rejected");
        let text = err.to_string();
        assert!(
            text.contains("patch does not apply") || text.contains("patch failed"),
            "{text}"
        );
        assert!(!text.contains("Broken pipe"), "{text}");
    }

    #[test]
    fn input_run_applies_a_large_patch() {
        let tmp = scratch_repo();
        let dir = tmp.path();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        commit_all(dir, "init").unwrap();
        let mut patch = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1,20000 @@\n",
        );
        for i in 0..20000 {
            patch.push_str(&format!(
                "+line number {i} with some padding to grow the patch\n"
            ));
        }
        assert!(patch.len() > 65_536);
        run_git_with_input(
            dir,
            &["apply", "--cached", "-"],
            &patch,
            Duration::from_secs(30),
        )
        .unwrap();
        let staged = run_git(dir, &["diff", "--cached", "--numstat"]).unwrap();
        assert!(staged.starts_with("20000\t0\ta.txt"), "{staged}");
    }
}
