//! `multi worktree add`: an isolated worktree of the whole workspace in the
//! sibling `<workspace>-worktrees` directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::fsx;
use crate::git::{
    check_all_on_same_branch, check_branch_existence, create_and_switch_branch,
    expected_branch_for_repo, git_output, repo_uses_fixed_branch, run_git,
};
use crate::paths::Paths;
use crate::repos::{load_repos, Repository};
use crate::sync::{sync, SyncOptions};

fn create_root_worktree(
    root_dir: &Path,
    destination: &Path,
    branch_name: &str,
    base_ref: &str,
) -> Result<()> {
    let (exists_locally, exists_remotely) = check_branch_existence(root_dir, branch_name)?;
    let destination_str = destination.display().to_string();
    let remote_ref = format!("origin/{branch_name}");
    let args: Vec<&str> = if exists_locally {
        vec!["worktree", "add", &destination_str, branch_name]
    } else if exists_remotely {
        vec![
            "worktree",
            "add",
            "-b",
            branch_name,
            &destination_str,
            &remote_ref,
        ]
    } else {
        vec![
            "worktree",
            "add",
            "-b",
            branch_name,
            &destination_str,
            base_ref,
        ]
    };
    run_git(root_dir, &args)?;
    log::info!("Created worktree at {}", destination.display());
    Ok(())
}

fn resolve_relative_path(entry: &str) -> Result<PathBuf> {
    let relative = PathBuf::from(entry);
    let escapes = relative
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    if relative.is_absolute() || escapes {
        return Err(Error::Git(format!(
            "Worktree transfer path must be relative and stay inside the workspace: {entry}"
        )));
    }
    Ok(relative)
}

/// Sibling directory that holds all worktrees for a workspace:
/// `<parent>/<workspace>-worktrees/`.
pub fn worktrees_dir(root_dir: &Path) -> PathBuf {
    let name = fsx::file_name(root_dir);
    root_dir
        .parent()
        .unwrap_or(root_dir)
        .join(format!("{name}-worktrees"))
}

fn destination_for_name(root_dir: &Path, name: &str) -> Result<PathBuf> {
    let name_path = Path::new(name);
    let single_component = name_path.components().count() == 1
        && matches!(
            name_path.components().next(),
            Some(std::path::Component::Normal(_))
        );
    if name.is_empty()
        || name_path.is_absolute()
        || !single_component
        || name == "."
        || name == ".."
    {
        return Err(Error::Git(
            "Worktree NAME must be a single directory name. Use --branch for branch names that contain path separators."
                .to_string(),
        ));
    }
    Ok(worktrees_dir(root_dir).join(name))
}

fn repo_for_path(paths: &Paths, relative_path: &Path) -> Result<(PathBuf, PathBuf)> {
    let source = fsx::resolve(&paths.root_dir.join(relative_path));
    for repo in load_repos(paths)? {
        let repo_root = fsx::resolve(&repo.path);
        if let Ok(inside) = source.strip_prefix(&repo_root) {
            return Ok((repo.path.clone(), inside.to_path_buf()));
        }
    }
    Ok((paths.root_dir.clone(), relative_path.to_path_buf()))
}

fn is_gitignored(repo_path: &Path, relative_path: &Path) -> Result<bool> {
    let relative = relative_path.display().to_string();
    let output = git_output(repo_path, &["check-ignore", "--quiet", "--", &relative])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::Git(format!(
            "Could not check gitignore status for {}",
            repo_path.join(relative_path).display()
        ))),
    }
}

fn validate_transfer_source(paths: &Paths, relative_path: &Path) -> Result<PathBuf> {
    let source = paths.root_dir.join(relative_path);
    if !source.exists() {
        return Err(Error::Git(format!(
            "Configured worktree transfer path does not exist: {}",
            relative_path.display()
        )));
    }
    let (repo_path, repo_relative) = repo_for_path(paths, relative_path)?;
    if !is_gitignored(&repo_path, &repo_relative)? {
        return Err(Error::Git(format!(
            "Configured worktree transfer path is not gitignored: {}",
            relative_path.display()
        )));
    }
    Ok(source)
}

fn transfer_path(source: &Path, destination: &Path, transfer_type: &str) -> Result<()> {
    if destination.exists() || fsx::is_symlink(destination) {
        log::info!("Skipping existing worktree path {}", destination.display());
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fsx::create_dir_all(parent)?;
    }
    if transfer_type == "symlink" {
        fsx::symlink(source, destination)?;
        log::info!(
            "Symlinked {} -> {}",
            destination.display(),
            source.display()
        );
    } else if source.is_dir() {
        fsx::copy_recursively(source, destination)?;
        log::info!(
            "Copied directory {} -> {}",
            source.display(),
            destination.display()
        );
    } else {
        fsx::copy_recursively(source, destination)?;
        log::info!(
            "Copied file {} -> {}",
            source.display(),
            destination.display()
        );
    }
    Ok(())
}

fn transfer_configured_paths(source_paths: &Paths, destination_paths: &Paths) -> Result<()> {
    let settings = source_paths.settings()?;
    for transfer_type in ["symlink", "copy"] {
        for entry in settings.worktree_transfer_paths(transfer_type)? {
            let relative = resolve_relative_path(&entry)?;
            let source = validate_transfer_source(source_paths, &relative)?;
            let destination = destination_paths.root_dir.join(&relative);
            transfer_path(&source, &destination, transfer_type)?;
        }
    }
    Ok(())
}

fn checkout_subrepos(
    source_repos: &[Repository],
    destination_repos: &[Repository],
    branch_name: &str,
    allow_create: bool,
    base_ref: &str,
) -> Result<()> {
    let source_by_name: HashMap<&str, &Repository> =
        source_repos.iter().map(|r| (r.name.as_str(), r)).collect();
    for repo in destination_repos {
        let repo_branch = expected_branch_for_repo(repo, Some(branch_name))
            .expect("workspace branch is always set");
        if repo_uses_fixed_branch(repo) {
            create_and_switch_branch(&repo.path, &repo_branch, allow_create)?;
            continue;
        }
        if let Some(source_repo) = source_by_name.get(repo.name.as_str()) {
            if base_ref != "HEAD" && !check_branch_existence(&repo.path, &repo_branch)?.0 {
                let source_path = source_repo.path.display().to_string();
                let fetched =
                    run_git(&repo.path, &["fetch", &source_path, base_ref]).and_then(|_| {
                        run_git(&repo.path, &["checkout", "-b", &repo_branch, "FETCH_HEAD"])
                    });
                match fetched {
                    Ok(_) => {
                        log::info!("✅ Switched to branch '{repo_branch}' in {}", repo.path.display());
                        continue;
                    }
                    Err(err) => log::warn!(
                        "⚠️  Base ref {base_ref} was not available for {}; creating {repo_branch} from the current checkout instead. {err}",
                        repo.name
                    ),
                }
            }
        }
        create_and_switch_branch(&repo.path, &repo_branch, allow_create)?;
    }
    Ok(())
}

/// Options for [`add_worktree`].
#[derive(Debug, Clone, Default)]
pub struct WorktreeAddOptions {
    /// Branch for the worktree; defaults to the worktree name.
    pub branch_name: Option<String>,
    pub install_set: Option<String>,
    /// Base ref for newly created root and sub-repo branches (default `HEAD`).
    pub base_ref: Option<String>,
}

/// Create `<parent>/<workspace>-worktrees/<name>`, sync it, check sub-repos
/// out to the branch, and transfer configured gitignored paths.
pub fn add_worktree(root_dir: &Path, name: &str, options: &WorktreeAddOptions) -> Result<PathBuf> {
    let paths = Paths::discover(root_dir, options.install_set.as_deref())?;
    if paths.settings()?.is_monorepo() {
        return Err(Error::Usage(
            "The 'multi worktree add' command is not available in monorepo mode. Use git directly in the root workspace."
                .to_string(),
        ));
    }
    let branch_name = options
        .branch_name
        .clone()
        .unwrap_or_else(|| name.to_string());
    let base_ref = options
        .base_ref
        .clone()
        .unwrap_or_else(|| "HEAD".to_string());
    let destination = destination_for_name(&paths.root_dir, name)?;
    if destination.exists() {
        return Err(Error::Git(format!(
            "Worktree destination already exists: {}",
            destination.display()
        )));
    }

    // No clean-working-tree requirement: worktree creation branches from a
    // commit (HEAD or --base-ref) and never mutates the source working trees.
    let all_on_same_branch = check_all_on_same_branch(&paths, false)?;
    if !all_on_same_branch {
        log::warn!(
            "Some repos are not on their expected branches. If the target branches already exist for all repos, this command will fix the situation."
        );
    }

    if let Some(parent) = destination.parent() {
        fsx::create_dir_all(parent)?;
    }
    create_root_worktree(&paths.root_dir, &destination, &branch_name, &base_ref)?;
    // Skip branch matching during the initial sync: the worktree branch is
    // created in each sub-repo right after.
    sync(
        &destination,
        &SyncOptions {
            ensure_on_same_branch: false,
            install_set: options.install_set.clone(),
        },
    )?;

    let destination_paths = Paths::discover(&destination, options.install_set.as_deref())?;
    checkout_subrepos(
        &load_repos(&paths)?,
        &load_repos(&destination_paths)?,
        &branch_name,
        all_on_same_branch,
        &base_ref,
    )?;
    transfer_configured_paths(&paths, &destination_paths)?;
    Ok(destination)
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeEntry {
    pub path: String,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub prunable: bool,
}

/// Parse `git worktree list --porcelain`. The first entry is the main
/// worktree.
pub fn parse_worktree_list_porcelain(stdout: &str) -> Vec<GitWorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<GitWorktreeEntry> = None;
    for raw in stdout.split('\n') {
        let line = raw.trim_end();
        if line.is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(GitWorktreeEntry {
                path: path.to_string(),
                branch: None,
                detached: false,
                bare: false,
                prunable: false,
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        if let Some(branch) = line.strip_prefix("branch ") {
            entry.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if line == "detached" {
            entry.detached = true;
        } else if line == "bare" {
            entry.bare = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            entry.prunable = true;
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

/// Paths of the linked worktrees belonging to a trunk checkout. Empty when
/// the project is itself a linked worktree.
pub fn linked_worktree_paths(project_path: &Path, porcelain: &str) -> Vec<String> {
    let entries = parse_worktree_list_porcelain(porcelain);
    let Some(main) = entries.first() else {
        return Vec::new();
    };
    if fsx::resolve(Path::new(&main.path)) != fsx::resolve(project_path) {
        return Vec::new();
    }
    entries
        .iter()
        .skip(1)
        .filter(|e| !e.bare && !e.prunable)
        .map(|e| e.path.clone())
        .collect()
}

/// Linked worktrees of `project_path` that still exist on disk.
pub fn list_linked_worktree_paths(project_path: &Path) -> Vec<String> {
    if !project_path.join(".git").exists() {
        return Vec::new();
    }
    match run_git(project_path, &["worktree", "list", "--porcelain"]) {
        Ok(stdout) => linked_worktree_paths(project_path, &stdout)
            .into_iter()
            .filter(|p| Path::new(p).exists())
            .collect(),
        Err(err) => {
            log::warn!(
                "git worktree list failed for {}: {err}",
                project_path.display()
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUNK: &str = "/home/dev/my-workspace";
    const WORKTREE_A: &str = "/home/dev/my-workspace-worktrees/fix-login";
    const WORKTREE_B: &str = "/home/dev/my-workspace-worktrees/task-42";

    fn porcelain() -> String {
        [
            format!("worktree {TRUNK}"),
            "HEAD 1111111111111111111111111111111111111111".to_string(),
            "branch refs/heads/main".to_string(),
            String::new(),
            format!("worktree {WORKTREE_A}"),
            "HEAD 2222222222222222222222222222222222222222".to_string(),
            "branch refs/heads/fix-login".to_string(),
            String::new(),
            format!("worktree {WORKTREE_B}"),
            "HEAD 3333333333333333333333333333333333333333".to_string(),
            "detached".to_string(),
            String::new(),
        ]
        .join("\n")
    }

    #[test]
    fn parses_porcelain() {
        let entries = parse_worktree_list_porcelain(&porcelain());
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[1].path, WORKTREE_A);
        assert!(entries[2].detached);
        assert!(parse_worktree_list_porcelain("").is_empty());
        let no_trailing =
            parse_worktree_list_porcelain(&format!("worktree {TRUNK}\nbranch refs/heads/main"));
        assert_eq!(no_trailing.len(), 1);
        let bare = parse_worktree_list_porcelain("worktree /home/dev/bare.git\nbare\n");
        assert!(bare[0].bare);
        let prunable = parse_worktree_list_porcelain(
            "worktree /t\nbranch refs/heads/main\n\nworktree /tmp/stale\ndetached\nprunable gitdir file points to non-existent location\n",
        );
        assert!(prunable[1].prunable);
    }

    #[test]
    fn linked_paths() {
        assert_eq!(
            linked_worktree_paths(Path::new(TRUNK), &porcelain()),
            vec![WORKTREE_A.to_string(), WORKTREE_B.to_string()]
        );
        assert!(linked_worktree_paths(Path::new(WORKTREE_A), &porcelain()).is_empty());
        let with_stale = format!(
            "worktree {TRUNK}\nbranch refs/heads/main\n\nworktree /tmp/stale\ndetached\nprunable gone\n\nworktree {WORKTREE_A}\nbranch refs/heads/fix-login\n"
        );
        assert_eq!(
            linked_worktree_paths(Path::new(TRUNK), &with_stale),
            vec![WORKTREE_A.to_string()]
        );
        let with_bare = format!(
            "worktree {TRUNK}\nbranch refs/heads/main\n\nworktree /home/dev/bare.git\nbare\n"
        );
        assert!(linked_worktree_paths(Path::new(TRUNK), &with_bare).is_empty());
    }

    #[test]
    fn worktree_names_must_be_single_components() {
        let root = Path::new("/ws/root");
        assert_eq!(
            destination_for_name(root, "feat").unwrap(),
            PathBuf::from("/ws/root-worktrees/feat")
        );
        assert!(destination_for_name(root, "a/b").is_err());
        assert!(destination_for_name(root, "..").is_err());
        assert!(destination_for_name(root, "").is_err());
    }
}
