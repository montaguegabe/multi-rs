//! Workspace-level git hooks.
//!
//! A workspace opts in with `"hooks": {"path": ".githooks"}` in multi.json.
//! Multi then points `core.hooksPath` of the root repo and of every sub-repo at
//! that one directory, so a single workspace-level definition governs every
//! commit and push in the workspace, its worktrees and fresh clones.
//!
//! - The root repo gets the *relative* path. Git resolves a relative
//!   `core.hooksPath` against the working tree that runs the hook, and git
//!   worktrees share the root repo's config, so every worktree of the root
//!   (including `multi worktree add` worktrees) uses its own checkout's hooks.
//! - Sub-repos get the *absolute* path of the workspace's hooks directory.
//!   Each workspace (and each multi worktree) clones its own sub-repos, so the
//!   value is per workspace, and ad-hoc `git worktree add` checkouts of a
//!   sub-repo outside the workspace still run the workspace hooks.
//! - Sub-repos that are symlinks to a clone owned by another workspace are
//!   left alone: their owning workspace manages their hooks.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::fsx;
use crate::git::{git_config, git_config_get, is_git_repo_root};
use crate::paths::Paths;
use crate::repos::load_repos;

pub const ROOT_NAME: &str = "(root)";

/// Hook names git runs from core.hooksPath; used to flag non-executable
/// files, which git silently skips.
const GIT_HOOK_NAMES: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-auto-gc",
    "post-rewrite",
    "reference-transaction",
    "push-to-checkout",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookState {
    Installed,
    Missing,
    Mismatch,
    NotCloned,
    Symlinked,
}

impl HookState {
    pub fn as_str(self) -> &'static str {
        match self {
            HookState::Installed => "installed",
            HookState::Missing => "missing",
            HookState::Mismatch => "mismatch",
            HookState::NotCloned => "not-cloned",
            HookState::Symlinked => "symlinked",
        }
    }

    pub fn is_problem(self) -> bool {
        matches!(self, HookState::Missing | HookState::Mismatch)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoHookStatus {
    pub name: String,
    pub path: String,
    pub state: HookState,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HooksStatus {
    pub hooks_dir: String,
    pub hooks_dir_exists: bool,
    pub hooks: Vec<String>,
    pub non_executable: Vec<String>,
    pub repos: Vec<RepoHookStatus>,
    pub ok: bool,
}

impl HooksStatus {
    fn compute_ok(&self) -> bool {
        self.hooks_dir_exists
            && self.non_executable.is_empty()
            && !self.repos.iter().any(|repo| repo.state.is_problem())
    }
}

/// Workspace-relative hooks directory from multi.json, if configured.
pub fn configured_hooks_path(paths: &Paths) -> Result<Option<String>> {
    paths.settings()?.hooks_path()
}

struct Target {
    name: String,
    path: PathBuf,
    is_root: bool,
}

fn targets(paths: &Paths) -> Result<Vec<Target>> {
    let mut targets = vec![Target {
        name: ROOT_NAME.to_string(),
        path: paths.root_dir.clone(),
        is_root: true,
    }];
    if !paths.settings()?.is_monorepo() {
        for repo in load_repos(paths)? {
            targets.push(Target {
                name: repo.name,
                path: repo.path,
                is_root: false,
            });
        }
    }
    Ok(targets)
}

fn expected_value(paths: &Paths, relative_hooks_path: &str, is_root: bool) -> String {
    if is_root {
        relative_hooks_path.to_string()
    } else {
        fsx::resolve(&paths.root_dir.join(relative_hooks_path))
            .display()
            .to_string()
    }
}

fn require_hooks_path(paths: &Paths) -> Result<String> {
    configured_hooks_path(paths)?.ok_or_else(|| {
        Error::Usage(
            "No workspace hooks configured. Add \"hooks\": {\"path\": \".githooks\"} to multi.json."
                .to_string(),
        )
    })
}

fn list_hooks(hooks_dir: &Path) -> Result<(Vec<String>, Vec<String>)> {
    if !hooks_dir.is_dir() {
        return Ok((Vec::new(), Vec::new()));
    }
    let hooks: Vec<String> = fsx::sorted_entries(hooks_dir)?
        .into_iter()
        .filter(|entry| entry.is_file())
        .map(|entry| fsx::file_name(&entry))
        .filter(|name| GIT_HOOK_NAMES.contains(&name.as_str()))
        .collect();
    let non_executable = hooks
        .iter()
        .filter(|name| !fsx::is_executable(&hooks_dir.join(name)))
        .cloned()
        .collect();
    Ok((hooks, non_executable))
}

pub fn hooks_status(paths: &Paths) -> Result<HooksStatus> {
    let relative_hooks_path = require_hooks_path(paths)?;
    let hooks_dir = paths.root_dir.join(&relative_hooks_path);
    let (hooks, non_executable) = list_hooks(&hooks_dir)?;

    let mut repos = Vec::new();
    for target in targets(paths)? {
        let expected = expected_value(paths, &relative_hooks_path, target.is_root);
        let display_path = if target.is_root {
            ".".to_string()
        } else {
            target
                .path
                .strip_prefix(&paths.root_dir)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| target.path.display().to_string())
        };
        let (state, expected, actual) = if !target.is_root && fsx::is_symlink(&target.path) {
            (HookState::Symlinked, None, None)
        } else if !is_git_repo_root(&target.path) {
            (HookState::NotCloned, None, None)
        } else {
            let actual = git_config_get(&target.path, "core.hooksPath")?;
            let state = match &actual {
                None => HookState::Missing,
                Some(value) if *value == expected => HookState::Installed,
                Some(_) => HookState::Mismatch,
            };
            (state, Some(expected), actual)
        };
        repos.push(RepoHookStatus {
            name: target.name,
            path: display_path,
            state,
            expected,
            actual,
        });
    }

    let mut status = HooksStatus {
        hooks_dir: relative_hooks_path,
        hooks_dir_exists: hooks_dir.is_dir(),
        hooks,
        non_executable,
        repos,
        ok: false,
    };
    status.ok = status.compute_ok();
    Ok(status)
}

/// Point core.hooksPath of the root and every cloned sub-repo at the
/// workspace hooks directory. Returns the names of repos that changed.
/// A no-op when multi.json does not configure `hooks.path`.
pub fn install_hooks(paths: &Paths) -> Result<Vec<String>> {
    let Some(relative_hooks_path) = configured_hooks_path(paths)? else {
        return Ok(Vec::new());
    };
    let hooks_dir = paths.root_dir.join(&relative_hooks_path);
    if !hooks_dir.is_dir() {
        log::warn!(
            "Workspace hooks directory {relative_hooks_path} does not exist yet; git runs no hooks until it does."
        );
    }

    let mut changed = Vec::new();
    for target in targets(paths)? {
        if !target.is_root && fsx::is_symlink(&target.path) {
            log::debug!("Skipping hooks for symlinked repo {}", target.name);
            continue;
        }
        if !is_git_repo_root(&target.path) {
            continue;
        }
        let expected = expected_value(paths, &relative_hooks_path, target.is_root);
        let actual = git_config_get(&target.path, "core.hooksPath")?;
        if actual.as_deref() == Some(expected.as_str()) {
            continue;
        }
        if let Some(actual) = &actual {
            log::warn!(
                "{}: replacing core.hooksPath '{actual}' with the workspace hooks. Chain repo-local hooks from the workspace hooks instead.",
                target.name
            );
        }
        git_config(&target.path, &["core.hooksPath", &expected])?;
        changed.push(target.name);
    }

    if !changed.is_empty() {
        log::info!(
            "🪝 Installed workspace hooks ({relative_hooks_path}) in: {}",
            changed.join(", ")
        );
    }
    Ok(changed)
}

/// Unset core.hooksPath wherever it points at the workspace hooks.
pub fn uninstall_hooks(paths: &Paths) -> Result<Vec<String>> {
    let relative_hooks_path = require_hooks_path(paths)?;
    let mut removed = Vec::new();
    for target in targets(paths)? {
        if !target.is_root && fsx::is_symlink(&target.path) {
            continue;
        }
        if !is_git_repo_root(&target.path) {
            continue;
        }
        let expected = expected_value(paths, &relative_hooks_path, target.is_root);
        if git_config_get(&target.path, "core.hooksPath")?.as_deref() == Some(expected.as_str()) {
            git_config(&target.path, &["--unset", "core.hooksPath"])?;
            removed.push(target.name);
        }
    }
    Ok(removed)
}

/// Human-readable status report (the `multi hooks status` output).
pub fn format_status(status: &HooksStatus) -> String {
    let mut out = String::new();
    let dir_note = if status.hooks_dir_exists {
        ""
    } else {
        "  (MISSING)"
    };
    out.push_str(&format!(
        "Hooks directory: {}{dir_note}\n",
        status.hooks_dir
    ));
    let hooks = if status.hooks.is_empty() {
        "(none)".to_string()
    } else {
        status.hooks.join(", ")
    };
    out.push_str(&format!("Hooks: {hooks}\n"));
    if !status.non_executable.is_empty() {
        out.push_str(&format!(
            "Not executable (git skips them): {}\n",
            status.non_executable.join(", ")
        ));
    }
    let width = status.repos.iter().map(|r| r.path.len()).max().unwrap_or(0);
    for repo in &status.repos {
        let mut line = format!(
            "  {:<width$}  {}",
            repo.path,
            repo.state.as_str(),
            width = width
        );
        if repo.state == HookState::Mismatch {
            line.push_str(&format!(
                "  (core.hooksPath={}, expected {})",
                repo.actual.as_deref().unwrap_or(""),
                repo.expected.as_deref().unwrap_or("")
            ));
        }
        out.push_str(&line);
        out.push('\n');
    }
    if status.ok {
        out.push_str("✅ Workspace hooks are installed in every cloned repo.\n");
    } else {
        out.push_str("❌ Workspace hooks are not fully installed. Run `multi hooks install`.\n");
    }
    out
}
