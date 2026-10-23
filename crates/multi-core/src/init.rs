//! `multi init`: create `multi.json`, sync, commit.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::git::commit_all;
use crate::jsonc::write_json_object_2;
use crate::prompt::{confirm, prompt};
use crate::repo_urls::{derive_explicit_local_name, derive_repo_slug_from_url};
use crate::sync::{sync, SyncOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitRepoConfig {
    pub url: String,
    pub description: Option<String>,
    pub name: Option<String>,
}

impl InitRepoConfig {
    pub fn new(url: impl Into<String>) -> Self {
        InitRepoConfig {
            url: url.into(),
            description: None,
            name: None,
        }
    }
}

/// Interactively collect repository URLs and descriptions.
pub fn collect_repo_configs() -> Result<Vec<InitRepoConfig>> {
    let mut configs: Vec<InitRepoConfig> = Vec::new();
    let mut collect_descriptions = true;
    loop {
        let url = prompt("Enter a repository URL (or press Enter to finish)")?;
        if url.is_empty() {
            if configs.is_empty()
                && !confirm(
                    "No repositories added. Do you want to finish anyway?",
                    false,
                )?
            {
                continue;
            }
            break;
        }
        if collect_descriptions {
            let description =
                prompt("Enter a description for this repo (or press Enter to skip descriptions)")?;
            if description.is_empty() {
                collect_descriptions = false;
                for config in &mut configs {
                    config.description = None;
                }
                configs.push(InitRepoConfig::new(url));
            } else {
                configs.push(InitRepoConfig {
                    url,
                    description: Some(description),
                    name: None,
                });
            }
        } else {
            configs.push(InitRepoConfig::new(url));
        }
    }
    Ok(configs)
}

pub fn create_multi_json(root_dir: &Path, configs: &[InitRepoConfig]) -> Result<()> {
    let repos: Vec<Value> = configs
        .iter()
        .map(|config| {
            let mut entry = Map::new();
            entry.insert("url".to_string(), Value::String(config.url.clone()));
            if let Some(name) = &config.name {
                entry.insert("name".to_string(), Value::String(name.clone()));
            }
            if let Some(description) = &config.description {
                entry.insert(
                    "description".to_string(),
                    Value::String(description.clone()),
                );
            }
            Value::Object(entry)
        })
        .collect();
    let mut config = Map::new();
    config.insert("repos".to_string(), json!(repos));
    write_json_object_2(&root_dir.join("multi.json"), &config)
}

/// Shorten workspace-prefixed slugs to local names, skipping candidates that
/// would collide with another repo's slug or local name.
pub fn apply_default_local_names(
    configs: Vec<InitRepoConfig>,
    workspace_name: &str,
) -> Vec<InitRepoConfig> {
    if workspace_name.is_empty() {
        return configs;
    }
    let slugs: Vec<String> = configs
        .iter()
        .map(|c| derive_repo_slug_from_url(&c.url))
        .collect();
    let candidates: Vec<Option<String>> = configs
        .iter()
        .map(|c| derive_explicit_local_name(&c.url, workspace_name))
        .collect();
    let untouched_slugs: std::collections::HashSet<String> = slugs
        .iter()
        .zip(&candidates)
        .filter(|(_, candidate)| candidate.is_none())
        .map(|(slug, _)| slug.clone())
        .collect();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for candidate in candidates.iter().flatten() {
        *counts.entry(candidate.clone()).or_default() += 1;
    }
    let mut assigned: std::collections::HashSet<String> = std::collections::HashSet::new();
    configs
        .into_iter()
        .zip(candidates)
        .map(|(config, candidate)| match candidate {
            Some(candidate)
                if counts[&candidate] == 1
                    && !untouched_slugs.contains(&candidate)
                    && !assigned.contains(&candidate) =>
            {
                assigned.insert(candidate.clone());
                InitRepoConfig {
                    name: Some(candidate),
                    ..config
                }
            }
            _ => config,
        })
        .collect()
}

fn github_repo_slug_to_url(slug: &str, clone_protocol: &str) -> String {
    if clone_protocol == "ssh" {
        format!("git@github.com:{slug}.git")
    } else {
        format!("https://github.com/{slug}")
    }
}

fn create_github_repo(slug: &str, visibility: &str, description: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("gh");
    cmd.args(["repo", "create", slug, &format!("--{visibility}")]);
    if let Some(description) = description {
        cmd.args(["--description", description]);
    }
    let output = cmd.output().map_err(|_| {
        Error::Other("GitHub CLI `gh` is required when using --github-repo.".to_string())
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let message = if !stderr.is_empty() { stderr } else { stdout };
        return Err(Error::Other(format!(
            "Failed to create GitHub repo '{slug}': {message}"
        )));
    }
    Ok(())
}

/// Non-interactive inputs for `multi init`.
#[derive(Debug, Clone, Default)]
pub struct InitInputs {
    pub repo_urls: Vec<String>,
    pub repo_descriptions: Vec<String>,
    pub github_repos: Vec<String>,
    pub github_descriptions: Vec<String>,
    pub github_visibility: String,
    pub github_clone_protocol: String,
}

impl InitInputs {
    pub fn is_non_interactive(&self) -> bool {
        !self.repo_urls.is_empty() || !self.github_repos.is_empty()
    }
}

fn validate_description_count(
    descriptions: &[String],
    repo_count: usize,
    option: &str,
    target: &str,
) -> Result<()> {
    if !descriptions.is_empty() && descriptions.len() != repo_count {
        return Err(Error::Usage(format!(
            "{option} must be provided exactly once per {target}."
        )));
    }
    Ok(())
}

pub fn build_non_interactive_repo_configs(
    workspace_name: &str,
    inputs: &InitInputs,
) -> Result<Vec<InitRepoConfig>> {
    validate_description_count(
        &inputs.repo_descriptions,
        inputs.repo_urls.len(),
        "--repo-description",
        "--repo",
    )?;
    validate_description_count(
        &inputs.github_descriptions,
        inputs.github_repos.len(),
        "--github-description",
        "--github-repo",
    )?;

    let mut configs: Vec<InitRepoConfig> = inputs
        .repo_urls
        .iter()
        .enumerate()
        .map(|(index, url)| InitRepoConfig {
            url: url.clone(),
            description: inputs
                .repo_descriptions
                .get(index)
                .filter(|d| !d.is_empty())
                .cloned(),
            name: None,
        })
        .collect();

    for (index, slug) in inputs.github_repos.iter().enumerate() {
        if slug.matches('/').count() != 1 {
            return Err(Error::Usage(
                "--github-repo must be in OWNER/REPO format so multi can write the created remote URL to multi.json."
                    .to_string(),
            ));
        }
        let description = inputs
            .github_descriptions
            .get(index)
            .filter(|d| !d.is_empty())
            .cloned();
        create_github_repo(slug, &inputs.github_visibility, description.as_deref())?;
        configs.push(InitRepoConfig {
            url: github_repo_slug_to_url(slug, &inputs.github_clone_protocol),
            description,
            name: None,
        });
    }
    Ok(apply_default_local_names(configs, workspace_name))
}

/// Initialize a multi workspace in `root_dir`.
pub fn run_init(root_dir: &Path, inputs: &InitInputs) -> Result<()> {
    log::info!("Initializing multi workspace...");
    let workspace_name = crate::fsx::file_name(root_dir);
    let configs = if inputs.is_non_interactive() {
        build_non_interactive_repo_configs(&workspace_name, inputs)?
    } else {
        apply_default_local_names(collect_repo_configs()?, &workspace_name)
    };
    create_multi_json(root_dir, &configs)?;
    log::info!("Created multi.json configuration");
    sync(
        root_dir,
        &SyncOptions {
            ensure_on_same_branch: false,
            install_set: None,
        },
    )?;
    commit_all(root_dir, "Multi init: Configure multi workspace")?;
    log::info!("✅ Workspace initialized successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_prefixed_slugs_without_collisions() {
        let configs = vec![
            InitRepoConfig::new("https://github.com/org/t-ide-cli"),
            InitRepoConfig::new("https://github.com/org/t-ide-web"),
            InitRepoConfig::new("https://github.com/org/web"),
        ];
        let result = apply_default_local_names(configs, "t-ide");
        assert_eq!(result[0].name.as_deref(), Some("cli"));
        // "web" collides with the untouched slug of the third repo.
        assert_eq!(result[1].name, None);
        assert_eq!(result[2].name, None);
    }

    #[test]
    fn description_counts_are_validated() {
        let inputs = InitInputs {
            repo_urls: vec!["https://x/a".into(), "https://x/b".into()],
            repo_descriptions: vec!["only one".into()],
            github_visibility: "private".into(),
            github_clone_protocol: "https".into(),
            ..Default::default()
        };
        assert!(matches!(
            build_non_interactive_repo_configs("ws", &inputs),
            Err(Error::Usage(_))
        ));
    }
}
