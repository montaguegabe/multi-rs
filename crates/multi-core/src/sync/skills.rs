//! Expose sub-repo Codex skills at the workspace root.
//!
//! Codex discovers skills by walking *up* from the working directory: the
//! `.agents/skills` folder of the current directory, of its parent, and of the
//! repository root. It never looks down into nested directories, so a session
//! started at the workspace root cannot see a skill that lives in a sub-repo.
//! `multi sync` links every sub-repo `.agents/skills/<name>` into the root
//! `.agents/skills` so Codex finds them from the workspace.
//!
//! Claude Code is deliberately left alone: it already discovers nested
//! `.claude/skills` directories below the start directory (lazily, when a file
//! in that subdirectory is first read or edited). Re-check both tools'
//! discovery rules now and then. If Codex gains downward discovery this module
//! becomes redundant; if Claude Code loses it, link `.claude/skills` the same
//! way.
//!
//! Links are relative so they survive moving the workspace or checking it out
//! as a worktree. A link is recognised as multi-managed by its shape (`../../`
//! into a sub-repo's `.agents/skills`); anything else at the root, such as a
//! skill checked into the workspace repo itself, is never touched.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fsx;
use crate::paths::Paths;
use crate::repos::load_repos;

/// Where Codex looks for skills, relative to a directory.
pub const SKILLS_DIR: &str = ".agents/skills";
const SETTINGS_KEY: &str = "skills";

/// Enabled unless `multi.json` has `"skills": {"enabled": false}`.
pub fn is_skills_sync_enabled(paths: &Paths) -> Result<bool> {
    Ok(paths.settings()?.section_enabled(SETTINGS_KEY, true))
}

fn skills_dir(directory: &Path) -> PathBuf {
    directory.join(".agents").join("skills")
}

/// Skill directories (those holding a `SKILL.md`) under `skills_root`, by name.
fn skill_dirs(skills_root: &Path) -> Result<Vec<PathBuf>> {
    if !skills_root.is_dir() {
        return Ok(Vec::new());
    }
    Ok(fsx::sorted_entries(skills_root)?
        .into_iter()
        .filter(|path| path.join("SKILL.md").is_file())
        .collect())
}

/// Whether `path` is a link `multi` created into a sub-repo's skills.
pub fn is_managed_skill_link(path: &Path) -> bool {
    let Ok(target) = std::fs::read_link(path) else {
        return false;
    };
    let target = target.to_string_lossy().replace('\\', "/");
    target.starts_with("../../") && target.contains(&format!("/{SKILLS_DIR}/"))
}

/// Root link name and relative link target per sub-repo skill, in `multi.json`
/// order. The first repo to provide a name wins; later duplicates are reported
/// and skipped because a skill's directory name must match the name declared
/// in its `SKILL.md`, so renaming the link is not an option.
pub fn plan_skill_links(paths: &Paths) -> Result<Vec<(String, String)>> {
    let mut links: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for repo in load_repos(paths)? {
        let repo_rel = repo.relative_path.trim_end_matches('/').to_string();
        for skill_dir in skill_dirs(&skills_dir(&repo.path))? {
            let name = fsx::file_name(&skill_dir);
            if !seen.insert(name.clone()) {
                log::warn!(
                    "⚠️  Skill '{name}' in {} is shadowed by an earlier repo's skill of the same name; rename one of them to expose both.",
                    repo.name
                );
                continue;
            }
            links.push((
                name.clone(),
                format!("../../{repo_rel}/{SKILLS_DIR}/{name}"),
            ));
        }
    }
    Ok(links)
}

fn can_manage(destination: &Path) -> bool {
    !(destination.exists() || fsx::is_symlink(destination)) || is_managed_skill_link(destination)
}

/// `.gitignore` entries for the links `sync_all_skills` will maintain.
pub fn root_skill_link_entries(paths: &Paths) -> Result<Vec<String>> {
    if !is_skills_sync_enabled(paths)? {
        return Ok(Vec::new());
    }
    let destination_dir = skills_dir(&paths.root_dir);
    Ok(plan_skill_links(paths)?
        .into_iter()
        .filter(|(name, _)| can_manage(&destination_dir.join(name)))
        .map(|(name, _)| format!("/{SKILLS_DIR}/{name}"))
        .collect())
}

/// Link sub-repo `.agents/skills` entries into the root `.agents/skills`.
pub fn sync_all_skills(paths: &Paths) -> Result<()> {
    if !is_skills_sync_enabled(paths)? {
        log::debug!("Skills sync is disabled");
        return Ok(());
    }
    let destination_dir = skills_dir(&paths.root_dir);
    let planned = plan_skill_links(paths)?;
    let previously_managed: HashSet<String> = if destination_dir.is_dir() {
        fsx::sorted_entries(&destination_dir)?
            .into_iter()
            .filter(|path| is_managed_skill_link(path))
            .map(|path| fsx::file_name(&path))
            .collect()
    } else {
        HashSet::new()
    };
    if planned.is_empty() && previously_managed.is_empty() {
        log::debug!("No sub-repo Codex skills to link");
        return Ok(());
    }

    log::info!("Linking sub-repo Codex skills into the workspace root...");
    fsx::create_dir_all(&destination_dir)?;

    let mut written: HashSet<String> = HashSet::new();
    for (name, target) in planned {
        let destination = destination_dir.join(&name);
        if !can_manage(&destination) {
            log::warn!(
                "⚠️  Not linking skill '{name}': the workspace has its own entry at {SKILLS_DIR}/{name}."
            );
            continue;
        }
        let current = std::fs::read_link(&destination)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"));
        if current.as_deref() == Some(target.as_str()) {
            written.insert(name);
            continue;
        }
        if fsx::is_symlink(&destination) {
            fsx::remove_file(&destination)?;
        }
        fsx::symlink(Path::new(&target), &destination)?;
        log::info!("✅ Linked skill: {name} -> {target}");
        written.insert(name);
    }

    let mut stale: Vec<&String> = previously_managed.difference(&written).collect();
    stale.sort();
    for name in stale {
        fsx::remove_file(&destination_dir.join(name))?;
        log::info!("🗑️ Removed stale skill link: {name}");
    }

    log::info!("✅ Codex skills sync complete");
    Ok(())
}
