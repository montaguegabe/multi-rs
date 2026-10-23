//! Merge `launch.json` files.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::paths::{vscode_config_dir, Paths};
use crate::repos::Repository;

use super::helpers::{
    mark_required, merge_shared_file, prefix_repo_name_to_path, VscodeFileMerger,
};
use super::title_case;

/// Names of configurations marked required, directly or via a required
/// compound, in first-seen order.
pub fn get_required_launch_configurations(launch_json: &Map<String, Value>) -> Vec<String> {
    let mut required: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if !required.iter().any(|existing| existing == name) {
            required.push(name.to_string());
        }
    };
    if let Some(Value::Array(compounds)) = launch_json.get("compounds") {
        for compound in compounds {
            let is_required = compound
                .get("required")
                .map(crate::settings::truthy)
                .unwrap_or(false);
            if is_required {
                if let Some(Value::Array(names)) = compound.get("configurations") {
                    for name in names.iter().filter_map(Value::as_str) {
                        push(name);
                    }
                }
            }
        }
    }
    if let Some(Value::Array(configs)) = launch_json.get("configurations") {
        for config in configs {
            let is_required = config
                .get("required")
                .map(crate::settings::truthy)
                .unwrap_or(false);
            if is_required {
                if let Some(name) = config.get("name").and_then(Value::as_str) {
                    push(name);
                }
            }
        }
    }
    required
}

struct LaunchFileMerger<'a> {
    paths: &'a Paths,
}

impl VscodeFileMerger for LaunchFileMerger<'_> {
    fn paths(&self) -> &Paths {
        self.paths
    }

    fn destination_json_path(&self) -> PathBuf {
        self.paths.vscode_launch_path()
    }

    fn source_json_path(&self, repo_path: &Path) -> PathBuf {
        vscode_config_dir(repo_path).join("launch.json")
    }

    fn prepare_repo_json(
        &self,
        mut repo_json: Map<String, Value>,
        repo: &Repository,
    ) -> Result<Map<String, Value>> {
        // Add cwd to configurations unless explicitly provided.
        if let Some(Value::Array(configs)) = repo_json.get_mut("configurations") {
            for config in configs.iter_mut() {
                if let Value::Object(map) = config {
                    if !map.contains_key("cwd") {
                        map.insert(
                            "cwd".to_string(),
                            Value::String(prefix_repo_name_to_path(
                                "${workspaceFolder}",
                                &repo.relative_path,
                            )),
                        );
                    }
                }
            }
        }
        mark_required(
            repo_json.get_mut("compounds"),
            "name",
            &repo.required_compounds,
        );
        mark_required(
            repo_json.get_mut("configurations"),
            "name",
            &repo.required_launch_configurations,
        );
        Ok(repo_json)
    }

    fn post_process_json(&self, merged: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut merged =
            merge_shared_file(merged, &self.paths.vscode_launch_shared_path(), "launch");

        let required = get_required_launch_configurations(&merged);
        if required.is_empty() {
            return Ok(merged);
        }

        let master_name = title_case(&self.paths.root_name());
        let compounds = merged
            .entry("compounds".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(items) = compounds {
            // Rename any existing compound with the same name instead of removing it.
            for compound in items.iter_mut() {
                if compound.get("name").and_then(Value::as_str) == Some(master_name.as_str()) {
                    let renamed = format!("{master_name} (Original)");
                    log::info!("Renamed existing compound '{master_name}' to '{renamed}'");
                    if let Value::Object(map) = compound {
                        map.insert("name".to_string(), Value::String(renamed));
                    }
                }
            }
            let master_task_name = format!("All Required Tasks - {master_name}");
            log::debug!("Added preLaunchTask '{master_task_name}' to master compound");
            items.push(json!({
                "name": master_name,
                "configurations": required,
                "preLaunchTask": master_task_name,
            }));
            log::info!("Created/updated master compound '{master_name}' in launch.json");
        }
        Ok(merged)
    }
}

pub fn merge_launch_json(paths: &Paths) -> Result<()> {
    LaunchFileMerger { paths }.merge()
}
