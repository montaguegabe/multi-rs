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
//! into a `.agents/skills` directory). Only managed links whose target is no
//! longer a skill directory are removed, so a link to a repo outside the
//! active install set, or one a user made by hand to a valid skill, survives.
//! Anything else at the root, such as a skill checked into the workspace repo
//! itself, is never touched. Disabling the feature removes the managed links.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::fsx;
use crate::paths::Paths;
use crate::repos::load_repos_or_empty;

/// Where Codex looks for skills, relative to a directory.
pub const SKILLS_DIR: &str = ".agents/skills";
const SETTINGS_KEY: &str = "skills";

/// Enabled unless `multi.json` has `"skills": {"enabled": false}` (or
/// `"skills": false`).
pub fn is_skills_sync_enabled(paths: &Paths) -> Result<bool> {
    Ok(paths.settings()?.section_enabled(SETTINGS_KEY, true))
}

fn skills_dir(directory: &Path) -> PathBuf {
    directory.join(".agents").join("skills")
}

fn is_skill_dir(path: &Path) -> bool {
    path.join("SKILL.md").is_file()
}

/// Skill directories (those holding a `SKILL.md`) under `skills_root`, by name.
fn skill_dirs(skills_root: &Path) -> Result<Vec<PathBuf>> {
    if !skills_root.is_dir() {
        return Ok(Vec::new());
    }
    Ok(fsx::sorted_entries(skills_root)?
        .into_iter()
        .filter(|path| is_skill_dir(path))
        .collect())
}

fn link_target_text(path: &Path) -> Option<String> {
    std::fs::read_link(path)
        .ok()
        .map(|target| target.to_string_lossy().replace('\\', "/"))
}

/// Whether `path` is a link of the shape `multi` creates into a sub-repo's
/// skills.
pub fn is_managed_skill_link(path: &Path) -> bool {
    link_target_text(path).is_some_and(|target| {
        target.starts_with("../../") && target.contains(&format!("/{SKILLS_DIR}/"))
    })
}

/// A managed link whose target is no longer a skill directory.
fn is_stale_skill_link(path: &Path) -> bool {
    is_managed_skill_link(path) && !is_skill_dir(path)
}

/// Root links to create: `(name, relative target)` in `multi.json` order,
/// plus the `(name, repo)` pairs skipped because an earlier repo already
/// provides that name. A skill's directory name must match the name declared
/// in its `SKILL.md`, so renaming a link is not an option.
#[derive(Debug, Default)]
pub struct SkillLinkPlan {
    pub links: Vec<(String, String)>,
    pub shadowed: Vec<(String, String)>,
}

pub fn plan_skill_links(paths: &Paths) -> Result<SkillLinkPlan> {
    let mut plan = SkillLinkPlan::default();
    let mut seen: HashSet<String> = HashSet::new();
    for repo in load_repos_or_empty(paths)? {
        let repo_rel = repo.relative_path.trim_end_matches('/').to_string();
        for skill_dir in skill_dirs(&skills_dir(&repo.path))? {
            let name = fsx::file_name(&skill_dir);
            if !seen.insert(name.clone()) {
                plan.shadowed.push((name, repo.name.clone()));
                continue;
            }
            plan.links.push((
                name.clone(),
                format!("../../{repo_rel}/{SKILLS_DIR}/{name}"),
            ));
        }
    }
    Ok(plan)
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
        .links
        .into_iter()
        .filter(|(name, _)| can_manage(&destination_dir.join(name)))
        .map(|(name, _)| format!("/{SKILLS_DIR}/{name}"))
        .collect())
}

fn managed_links(destination_dir: &Path) -> Result<Vec<PathBuf>> {
    if !destination_dir.is_dir() {
        return Ok(Vec::new());
    }
    Ok(fsx::sorted_entries(destination_dir)?
        .into_iter()
        .filter(|path| is_managed_skill_link(path))
        .collect())
}

fn remove_links(links: &[PathBuf], reason: &str) -> Result<()> {
    for link in links {
        fsx::remove_file(link)?;
        log::info!("🗑️ Removed skill link {}: {reason}", fsx::file_name(link));
    }
    Ok(())
}

/// Link sub-repo `.agents/skills` entries into the root `.agents/skills`.
pub fn sync_all_skills(paths: &Paths) -> Result<()> {
    let destination_dir = skills_dir(&paths.root_dir);
    if !is_skills_sync_enabled(paths)? {
        log::debug!("Skills sync is disabled");
        return remove_links(&managed_links(&destination_dir)?, "skills sync is disabled");
    }
    let plan = plan_skill_links(paths)?;
    for (name, repo) in &plan.shadowed {
        log::warn!(
            "⚠️  Skill '{name}' in {repo} is shadowed by an earlier repo's skill of the same name; rename one of them to expose both."
        );
    }
    let stale: Vec<PathBuf> = managed_links(&destination_dir)?
        .into_iter()
        .filter(|path| is_stale_skill_link(path))
        .collect();
    if plan.links.is_empty() && stale.is_empty() {
        log::debug!("No sub-repo Codex skills to link");
        return Ok(());
    }

    log::info!("Linking sub-repo Codex skills into the workspace root...");
    fsx::create_dir_all(&destination_dir)?;
    for (name, target) in plan.links {
        let destination = destination_dir.join(&name);
        if !can_manage(&destination) {
            log::warn!(
                "⚠️  Not linking skill '{name}': the workspace has its own entry at {SKILLS_DIR}/{name}."
            );
            continue;
        }
        if link_target_text(&destination).as_deref() == Some(target.as_str()) {
            continue;
        }
        if fsx::is_symlink(&destination) {
            fsx::remove_file(&destination)?;
        }
        fsx::symlink(Path::new(&target), &destination)?;
        log::info!("✅ Linked skill: {name} -> {target}");
    }
    // Re-evaluate after linking: a stale link that the plan just re-pointed
    // is no longer stale.
    let stale: Vec<PathBuf> = stale
        .into_iter()
        .filter(|path| is_stale_skill_link(path))
        .collect();
    remove_links(&stale, "its target is no longer a skill")?;

    log::info!("✅ Codex skills sync complete");
    Ok(())
}
