//! Sync GitHub Actions workflows from monorepo package directories into the
//! root `.github/workflows` directory.

use std::collections::HashSet;
use std::path::Path;

use serde_yaml::{Mapping, Value};

use crate::error::{Error, Result};
use crate::fsx;
use crate::paths::Paths;
use crate::repos::load_repos;

pub const GENERATED_MARKER: &str = "# multi-generated: github-workflow";
const WORKFLOW_EXTENSIONS: &[&str] = &["yml", "yaml"];

fn is_generated_workflow(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.lines().next().map(str::trim) == Some(GENERATED_MARKER))
        .unwrap_or(false)
}

fn repo_slug(repo_name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for c in repo_name.trim_matches('/').chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            slug.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "repo".to_string()
    } else {
        slug
    }
}

fn load_workflow(source_path: &Path) -> Result<Mapping> {
    let content = fsx::read_to_string(source_path)?;
    let parsed: Value = serde_yaml::from_str(&content).map_err(|_| {
        Error::Config(format!(
            "Failed to parse workflow YAML: {}",
            source_path.display()
        ))
    })?;
    match parsed {
        Value::Null => Ok(Mapping::new()),
        Value::Mapping(map) => Ok(map),
        _ => Err(Error::Config(format!(
            "Workflow file must contain a top-level object: {}",
            source_path.display()
        ))),
    }
}

fn key(name: &str) -> Value {
    Value::String(name.to_string())
}

fn inject_job_working_directory(workflow: &mut Mapping, repo_name: &str) {
    let Some(Value::Mapping(jobs)) = workflow.get_mut(key("jobs")) else {
        return;
    };
    for (_, job) in jobs.iter_mut() {
        let Value::Mapping(job) = job else { continue };
        if !job.contains_key(key("defaults")) || job.get(key("defaults")) == Some(&Value::Null) {
            job.insert(key("defaults"), Value::Mapping(Mapping::new()));
        }
        let Some(Value::Mapping(defaults)) = job.get_mut(key("defaults")) else {
            continue;
        };
        if !defaults.contains_key(key("run")) || defaults.get(key("run")) == Some(&Value::Null) {
            defaults.insert(key("run"), Value::Mapping(Mapping::new()));
        }
        if let Some(Value::Mapping(run)) = defaults.get_mut(key("run")) {
            if !run.contains_key(key("working-directory")) {
                run.insert(
                    key("working-directory"),
                    Value::String(repo_name.to_string()),
                );
            }
        }
    }
}

fn is_filename_available(
    destination_dir: &Path,
    filename: &str,
    reserved: &HashSet<String>,
) -> bool {
    if reserved.contains(filename) {
        return false;
    }
    let destination = destination_dir.join(filename);
    if !destination.exists() {
        return true;
    }
    is_generated_workflow(&destination)
}

fn choose_destination_filename(
    destination_dir: &Path,
    source_filename: &str,
    repo_name: &str,
    reserved: &HashSet<String>,
) -> String {
    if is_filename_available(destination_dir, source_filename, reserved) {
        return source_filename.to_string();
    }
    let slug = repo_slug(repo_name);
    let prefixed = format!("{slug}--{source_filename}");
    if is_filename_available(destination_dir, &prefixed, reserved) {
        return prefixed;
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{slug}-{counter}--{source_filename}");
        if is_filename_available(destination_dir, &candidate, reserved) {
            return candidate;
        }
        counter += 1;
    }
}

fn render_generated_workflow(
    workflow: &Mapping,
    source_display: &str,
    repo_name: &str,
) -> Result<String> {
    let serialized = serde_yaml::to_string(&Value::Mapping(workflow.clone()))
        .map_err(|e| Error::Other(format!("Failed to serialize workflow: {e}")))?;
    Ok(format!(
        "{GENERATED_MARKER}\n# source: {source_display}\n# repo: {repo_name}\n\n{serialized}"
    ))
}

/// Sync GitHub Actions workflow files from monorepo directories to root.
pub fn sync_all_github_actions(paths: &Paths) -> Result<()> {
    if !paths.settings()?.is_monorepo() {
        log::debug!("Skipping GitHub Actions sync outside monorepo mode");
        return Ok(());
    }
    log::info!("Syncing GitHub Actions workflows...");
    let repos = load_repos(paths)?;
    let destination_dir = paths.root_dir.join(".github").join("workflows");
    fsx::create_dir_all(&destination_dir)?;

    let previously_generated: HashSet<String> =
        fsx::files_with_extensions(&destination_dir, WORKFLOW_EXTENSIONS)?
            .into_iter()
            .filter(|path| is_generated_workflow(path))
            .map(|path| fsx::file_name(&path))
            .collect();

    let mut reserved: HashSet<String> = HashSet::new();
    let mut written: HashSet<String> = HashSet::new();

    for repo in &repos {
        let source_dir = repo.path.join(".github").join("workflows");
        if !source_dir.exists() {
            log::debug!("No workflows directory found for {}", repo.name);
            continue;
        }
        for source_path in fsx::files_with_extensions(&source_dir, WORKFLOW_EXTENSIONS)? {
            let source_filename = fsx::file_name(&source_path);
            let destination_filename = choose_destination_filename(
                &destination_dir,
                &source_filename,
                &repo.name,
                &reserved,
            );
            reserved.insert(destination_filename.clone());
            written.insert(destination_filename.clone());

            let mut workflow = load_workflow(&source_path)?;
            inject_job_working_directory(&mut workflow, &repo.name);

            let source_display = source_path
                .strip_prefix(&paths.root_dir)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| source_path.display().to_string());
            let content = render_generated_workflow(&workflow, &source_display, &repo.name)?;
            fsx::write(&destination_dir.join(&destination_filename), content)?;
            log::info!("✅ Synced workflow: {destination_filename}");
        }
    }

    for stale in previously_generated.difference(&written) {
        let stale_path = destination_dir.join(stale);
        if stale_path.exists() && is_generated_workflow(&stale_path) {
            fsx::remove_file(&stale_path)?;
            log::info!("🗑️ Removed stale generated workflow: {stale}");
        }
    }

    log::info!("✅ GitHub Actions workflow sync complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(repo_slug("packages/web"), "packages-web");
        assert_eq!(repo_slug("/a b/"), "a-b");
        assert_eq!(repo_slug("///"), "repo");
    }

    #[test]
    fn injects_working_directory() {
        let mut workflow: Mapping = serde_yaml::from_str(
            "jobs:\n  build:\n    steps: []\n  test:\n    defaults:\n      run:\n        working-directory: custom\n",
        )
        .unwrap();
        inject_job_working_directory(&mut workflow, "packages/web");
        let rendered = serde_yaml::to_string(&Value::Mapping(workflow)).unwrap();
        assert!(rendered.contains("working-directory: packages/web"));
        assert!(rendered.contains("working-directory: custom"));
    }
}
