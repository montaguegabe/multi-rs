//! `multi doctor`: diagnose common workspace configuration issues.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::git::{git_output, git_output_timeout, is_git_repo_root, run_git};
use crate::hooks::{configured_hooks_path, hooks_status};
use crate::paths::Paths;
use crate::repos::{load_repos, Repository};
use crate::sync::codex::codex_override_problem;

const REMOTE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default, Clone, Serialize)]
pub struct DoctorReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub infos: Vec<String>,
}

impl DoctorReport {
    pub fn should_fail(&self, strict: bool) -> bool {
        !self.errors.is_empty() || (strict && !self.warnings.is_empty())
    }
}

/// Options controlling which checks run.
#[derive(Debug, Clone)]
pub struct DoctorOptions {
    /// Run `git ls-remote` against every declared remote (slow, needs network).
    pub check_remotes: bool,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        DoctorOptions {
            check_remotes: true,
        }
    }
}

/// In monorepo mode, configured directories that still contain a `.git`.
pub fn find_nested_git_repos_in_monorepo(paths: &Paths) -> Result<Vec<String>> {
    if !paths.settings()?.is_monorepo() {
        return Ok(Vec::new());
    }
    let repos = match load_repos(paths) {
        Ok(repos) => repos,
        Err(Error::NoRepositories(_)) | Err(Error::Config(_)) => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut nested: Vec<String> = repos
        .into_iter()
        .filter(|repo| repo.path.join(".git").exists())
        .map(|repo| repo.name)
        .collect();
    nested.sort();
    Ok(nested)
}

/// Immediate subdirectories of `root_dir` that are git repositories.
pub fn find_git_repos_on_disk(root_dir: &Path) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(root_dir) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() && !name.starts_with('.') && path.join(".git").is_dir() {
            result.insert(name);
        }
    }
    result
}

/// Sub-repos that have files tracked in the workspace git index.
pub fn find_tracked_subrepos(root_dir: &Path, repos: &[Repository]) -> Vec<String> {
    let Ok(output) = git_output(root_dir, &["ls-files", "--stage"]) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut tracked = BTreeSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        // "<mode> <object> <stage>\t<path>"
        let Some((meta, path)) = line.split_once('\t') else {
            continue;
        };
        let mode = meta.split_whitespace().next().unwrap_or("");
        for repo in repos {
            if path == repo.name || path.starts_with(&format!("{}/", repo.name)) {
                if mode != "160000" {
                    tracked.insert(repo.name.clone());
                }
                break;
            }
        }
    }
    tracked.into_iter().collect()
}

/// Sub-repos registered as git submodules in the workspace.
pub fn find_subrepo_submodules(root_dir: &Path, repos: &[Repository]) -> Vec<String> {
    let gitmodules = root_dir.join(".gitmodules");
    if !gitmodules.exists() {
        return Vec::new();
    }
    let Ok(output) = git_output(
        root_dir,
        &[
            "config",
            "-f",
            ".gitmodules",
            "--get-regexp",
            r"submodule\..*\.path",
        ],
    ) else {
        return Vec::new();
    };
    let submodule_paths: BTreeSet<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' ').map(|(_, p)| p.trim().to_string()))
        .collect();
    let mut result: Vec<String> = repos
        .iter()
        .filter(|repo| submodule_paths.contains(&repo.name))
        .map(|repo| repo.name.clone())
        .collect();
    result.sort();
    result
}

/// `None` when the remote is reachable, otherwise a short failure detail.
pub fn validate_repo_remote(url: &str) -> Option<String> {
    match git_output_timeout(Path::new("."), &["ls-remote", url], REMOTE_CHECK_TIMEOUT) {
        Err(err) => Some(format!("could not run git ls-remote: {err}")),
        Ok(None) => Some(format!(
            "timed out after {}s",
            REMOTE_CHECK_TIMEOUT.as_secs()
        )),
        Ok(Some(output)) => {
            if output.status.success() {
                return None;
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let text = if stderr.is_empty() { stdout } else { stderr };
            if let Some(first) = text.lines().next() {
                return Some(first.to_string());
            }
            Some(format!(
                "git ls-remote exited with status {}",
                output.status.code().unwrap_or(-1)
            ))
        }
    }
}

fn warn_if_root_git_missing(report: &mut DoctorReport, paths: &Paths) {
    if !is_git_repo_root(&paths.root_dir) {
        report.warnings.push(format!(
            "Workspace root is not a git repository at {}. Run `multi sync` (or `git init -b main`) to initialize it.",
            paths.root_dir.display()
        ));
    }
}

fn check_mode_specific_repo_requirements(
    report: &mut DoctorReport,
    paths: &Paths,
    repos: &[Repository],
    is_monorepo: bool,
) -> Result<()> {
    if is_monorepo {
        let nested = find_nested_git_repos_in_monorepo(paths)?;
        if !nested.is_empty() {
            report.warnings.push(format!(
                "monoRepo mode expects listed directories to be part of the root git repo (no nested .git). Found nested git repos: {}. Use standard multi-repo mode (monoRepo: false) for independent repos.",
                nested.join(", ")
            ));
        }
        return Ok(());
    }
    let missing_url: Vec<&str> = repos
        .iter()
        .filter(|repo| repo.url.is_none())
        .map(|repo| repo.name.as_str())
        .collect();
    if !missing_url.is_empty() {
        report.errors.push(format!(
            "These repos are missing `url` and cannot be managed in standard mode: {}.",
            missing_url.join(", ")
        ));
    }
    Ok(())
}

fn warn_on_missing_descriptions(report: &mut DoctorReport, repos: &[Repository]) {
    let mut missing: Vec<&str> = repos
        .iter()
        .filter(|repo| repo.description_text().is_none())
        .map(|repo| repo.name.as_str())
        .collect();
    missing.sort();
    if !missing.is_empty() {
        report.warnings.push(format!(
            "Repos declared in multi.json are missing a non-empty `description`: {}. Add descriptions so generated workspace context and diagnostics stay useful.",
            missing.join(", ")
        ));
    }
}

fn warn_on_unreachable_remotes(report: &mut DoctorReport, repos: &[Repository]) {
    let unreachable: Vec<String> = repos
        .iter()
        .filter_map(|repo| {
            let url = repo.url.as_ref()?;
            validate_repo_remote(url).map(|detail| format!("{} ({detail})", repo.name))
        })
        .collect();
    if !unreachable.is_empty() {
        report.warnings.push(format!(
            "Repos declared in multi.json have unreachable remotes: {}. Verify the URL, network access, and credentials.",
            unreachable.join(", ")
        ));
    }
}

fn warn_on_tracked_subrepos(
    report: &mut DoctorReport,
    paths: &Paths,
    repos: &[Repository],
    is_monorepo: bool,
) {
    if !is_git_repo_root(&paths.root_dir) || is_monorepo {
        return;
    }
    let tracked = find_tracked_subrepos(&paths.root_dir, repos);
    if !tracked.is_empty() {
        report.warnings.push(format!(
            "Sub-repos are tracked in the workspace git index (they should be .gitignored): {}. Add them to .gitignore and remove from the index with `git rm -r --cached <repo>`.",
            tracked.join(", ")
        ));
    }
}

fn warn_on_subrepo_submodules(report: &mut DoctorReport, paths: &Paths, repos: &[Repository]) {
    if !is_git_repo_root(&paths.root_dir) {
        return;
    }
    let submodules = find_subrepo_submodules(&paths.root_dir, repos);
    if !submodules.is_empty() {
        report.warnings.push(format!(
            "Sub-repos are registered as git submodules (multi does not use submodules): {}. Remove them with `git rm --cached <repo>` and delete the entry from .gitmodules.",
            submodules.join(", ")
        ));
    }
}

fn warn_on_repo_disk_mismatches(
    report: &mut DoctorReport,
    paths: &Paths,
    repos: &[Repository],
    is_monorepo: bool,
) {
    let declared: BTreeSet<String> = repos.iter().map(|repo| repo.name.clone()).collect();
    let on_disk = find_git_repos_on_disk(&paths.root_dir);
    let undeclared: Vec<&String> = on_disk.difference(&declared).collect();
    if !undeclared.is_empty() {
        report.warnings.push(format!(
            "Git repos found on disk but not declared in multi.json: {}. Add them to multi.json or remove them.",
            undeclared.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    if is_monorepo {
        let mut missing: Vec<&str> = repos
            .iter()
            .filter(|repo| !repo.path.is_dir())
            .map(|repo| repo.name.as_str())
            .collect();
        missing.sort();
        if !missing.is_empty() {
            report.warnings.push(format!(
                "Repos declared in multi.json but not found on disk: {}. In monoRepo mode, create these directories in the workspace.",
                missing.join(", ")
            ));
        }
        return;
    }
    let missing: Vec<&String> = declared.difference(&on_disk).collect();
    if !missing.is_empty() {
        report.warnings.push(format!(
            "Repos declared in multi.json but not found on disk: {}. Run `multi sync` to clone them.",
            missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
}

fn warn_on_uninstalled_hooks(report: &mut DoctorReport, paths: &Paths) -> Result<()> {
    match configured_hooks_path(paths) {
        Ok(None) => return Ok(()),
        Ok(Some(_)) => {}
        Err(Error::Config(message)) => {
            report.errors.push(message);
            return Ok(());
        }
        Err(err) => return Err(err),
    }
    let status = hooks_status(paths)?;
    if !status.hooks_dir_exists {
        report.warnings.push(format!(
            "Workspace hooks directory {} does not exist; git runs no hooks.",
            status.hooks_dir
        ));
    }
    if !status.non_executable.is_empty() {
        report.warnings.push(format!(
            "Workspace hooks are not executable (git skips them): {}. Run `chmod +x` on them.",
            status.non_executable.join(", ")
        ));
    }
    let uninstalled: Vec<&str> = status
        .repos
        .iter()
        .filter(|repo| repo.state.is_problem())
        .map(|repo| repo.path.as_str())
        .collect();
    if !uninstalled.is_empty() {
        report.warnings.push(format!(
            "Workspace hooks are not installed in: {}. Run `multi hooks install`.",
            uninstalled.join(", ")
        ));
    }
    Ok(())
}

/// Run every diagnostic for the workspace containing `target_dir`.
fn warn_on_codex_override_problems(report: &mut DoctorReport, paths: &Paths) -> Result<()> {
    if let Some(problem) = codex_override_problem(paths)? {
        report.warnings.push(problem);
    }
    Ok(())
}

pub fn run_doctor_checks(target_dir: &Path, options: &DoctorOptions) -> DoctorReport {
    let mut report = DoctorReport::default();
    let paths = match Paths::discover(target_dir, None) {
        Ok(paths) => paths,
        Err(_) => {
            report.errors.push(
                "Could not find multi.json in this directory or parent directories.".to_string(),
            );
            return report;
        }
    };
    report.infos.push(format!(
        "Found multi.json at {}",
        paths.multi_json_path().display()
    ));

    let settings = match paths.settings() {
        Ok(settings) => settings,
        Err(err) => {
            report
                .errors
                .push(format!("Could not parse multi.json: {err}"));
            return report;
        }
    };
    let is_monorepo = settings.is_monorepo();

    warn_if_root_git_missing(&mut report, &paths);

    let repos = match load_repos(&paths) {
        Ok(repos) => repos,
        Err(err) => {
            report.errors.push(err.to_string());
            return report;
        }
    };

    let checks: Result<()> = (|| {
        check_mode_specific_repo_requirements(&mut report, &paths, &repos, is_monorepo)?;
        warn_on_missing_descriptions(&mut report, &repos);
        if options.check_remotes {
            warn_on_unreachable_remotes(&mut report, &repos);
        }
        warn_on_tracked_subrepos(&mut report, &paths, &repos, is_monorepo);
        warn_on_subrepo_submodules(&mut report, &paths, &repos);
        warn_on_repo_disk_mismatches(&mut report, &paths, &repos, is_monorepo);
        warn_on_uninstalled_hooks(&mut report, &paths)?;
        warn_on_codex_override_problems(&mut report, &paths)?;
        Ok(())
    })();
    if let Err(err) = checks {
        report.errors.push(err.to_string());
    }
    report
}

/// Apply safe fixes: remove tracked sub-repos from the root git index.
/// Returns the names of repos that were fixed.
pub fn run_doctor_fixes(target_dir: &Path) -> Result<Vec<String>> {
    let paths = Paths::discover(target_dir, None)?;
    if !is_git_repo_root(&paths.root_dir) || paths.settings()?.is_monorepo() {
        return Ok(Vec::new());
    }
    let repos = load_repos(&paths)?;
    let tracked = find_tracked_subrepos(&paths.root_dir, &repos);
    if tracked.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["rm", "-r", "--cached", "--"];
    args.extend(tracked.iter().map(String::as_str));
    run_git(&paths.root_dir, &args)?;
    Ok(tracked)
}

/// Log a report the way the CLI prints it.
pub fn log_report(report: &DoctorReport) {
    log::info!("Running diagnostics...");
    for info in &report.infos {
        log::info!("{info}");
    }
    for warning in &report.warnings {
        log::warn!("{warning}");
    }
    for error in &report.errors {
        log::error!("{error}");
    }
    if report.errors.is_empty() && report.warnings.is_empty() {
        log::info!("✅ No issues found.");
    }
}
