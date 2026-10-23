//! Generate `AGENTS.md` and `CLAUDE.md` from `AGENTS.parts/*.md`.

use std::path::Path;

use crate::error::Result;
use crate::fsx;
use crate::paths::Paths;
use crate::repos::load_repos;

fn repo_descriptions_section(paths: &Paths) -> Result<String> {
    let settings = paths.settings()?;
    if !settings.agent_instructions().include_repo_descriptions || !settings.has_repos() {
        return Ok(String::new());
    }
    let described: Vec<(String, String)> = load_repos(paths)?
        .into_iter()
        .filter_map(|repo| {
            repo.description
                .clone()
                .filter(|d| !d.is_empty())
                .map(|d| (repo.name.clone(), d))
        })
        .collect();
    if described.is_empty() {
        return Ok(String::new());
    }
    let mut lines = vec![
        "This workspace contains multiple repositories:".to_string(),
        String::new(),
    ];
    for (name, description) in described {
        lines.push(format!("- `{name}`: {description}"));
    }
    Ok(lines.join("\n"))
}

pub fn read_agents_part_files(parts_dir: &Path) -> Result<Vec<String>> {
    if !parts_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
    for part in fsx::files_with_extensions(parts_dir, &["md"])? {
        let content = fsx::read_to_string(&part)?;
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    Ok(parts)
}

pub fn build_agents_content(
    repo_dir: &Path,
    paths: &Paths,
    include_root_descriptions: bool,
) -> Result<String> {
    let parts_dir_name = paths.settings()?.agent_instructions().parts_dir;
    let mut parts = read_agents_part_files(&repo_dir.join(parts_dir_name))?;
    if include_root_descriptions {
        let descriptions = repo_descriptions_section(paths)?;
        if !descriptions.is_empty() {
            parts.insert(0, descriptions);
        }
    }
    if parts.is_empty() {
        return Ok(String::new());
    }
    Ok(format!("{}\n", parts.join("\n\n").trim_end()))
}

pub fn would_generate_agents_for_repo(
    repo_dir: &Path,
    paths: &Paths,
    is_root: bool,
) -> Result<bool> {
    if !paths.settings()?.agent_instructions().enabled {
        return Ok(false);
    }
    Ok(!build_agents_content(repo_dir, paths, is_root)?.is_empty())
}

pub fn generate_agents_files(repo_dir: &Path, paths: &Paths, is_root: bool) -> Result<()> {
    let content = build_agents_content(repo_dir, paths, is_root)?;
    if content.is_empty() {
        log::debug!("No AGENTS parts found at {}", repo_dir.display());
        return Ok(());
    }
    fsx::write(&repo_dir.join("AGENTS.md"), &content)?;
    fsx::write(&repo_dir.join("CLAUDE.md"), &content)?;
    log::info!(
        "✅ Generated AGENTS.md and CLAUDE.md at {}",
        repo_dir.display()
    );
    Ok(())
}

/// Generate agent files for the root and each cloned sub-repo when enabled.
pub fn sync_all_agents(paths: &Paths) -> Result<()> {
    let settings = paths.settings()?;
    if !settings.agent_instructions().enabled {
        log::debug!("AGENTS generation is disabled");
        return Ok(());
    }
    generate_agents_files(&paths.root_dir, paths, true)?;
    if settings.is_monorepo() || !settings.has_repos() {
        return Ok(());
    }
    for repo in load_repos(paths)? {
        if repo.path.exists() {
            generate_agents_files(&repo.path, paths, false)?;
        }
    }
    Ok(())
}
