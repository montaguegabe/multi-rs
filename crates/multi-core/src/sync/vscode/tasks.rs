//! Merge `tasks.json` files.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::error::Result;
use crate::paths::{vscode_config_dir, Paths};
use crate::repos::Repository;

use super::helpers::{mark_required, merge_shared_file, VscodeFileMerger};
use super::title_case;

/// Labels of tasks marked `"required": true`, in first-seen order.
pub fn get_required_tasks(tasks_json: &Map<String, Value>) -> Vec<String> {
    let mut required: Vec<String> = Vec::new();
    if let Some(Value::Array(tasks)) = tasks_json.get("tasks") {
        for task in tasks {
            let is_required = task
                .get("required")
                .map(crate::settings::truthy)
                .unwrap_or(false);
            if is_required {
                if let Some(label) = task.get("label").and_then(Value::as_str) {
                    if !required.iter().any(|existing| existing == label) {
                        required.push(label.to_string());
                    }
                }
            }
        }
    }
    required
}

struct TasksFileMerger<'a> {
    paths: &'a Paths,
}

impl VscodeFileMerger for TasksFileMerger<'_> {
    fn paths(&self) -> &Paths {
        self.paths
    }

    fn destination_json_path(&self) -> PathBuf {
        self.paths.vscode_tasks_path()
    }

    fn source_json_path(&self, repo_path: &Path) -> PathBuf {
        vscode_config_dir(repo_path).join("tasks.json")
    }

    fn repo_defaults(&self, _repo: &Repository) -> Option<Value> {
        Some(json!({
            "tasks": {"apply_to_list_items": {"options": {"cwd": "${workspaceFolder}"}}}
        }))
    }

    fn prepare_repo_json(
        &self,
        mut repo_json: Map<String, Value>,
        repo: &Repository,
    ) -> Result<Map<String, Value>> {
        mark_required(repo_json.get_mut("tasks"), "label", &repo.required_tasks);
        Ok(repo_json)
    }

    fn post_process_json(&self, merged: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut merged = merge_shared_file(merged, &self.paths.vscode_tasks_shared_path(), "tasks");

        let required = get_required_tasks(&merged);
        if required.is_empty() {
            return Ok(merged);
        }

        let master_name = format!(
            "All Required Tasks - {}",
            title_case(&self.paths.root_name())
        );
        let tasks = merged
            .entry("tasks".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(items) = tasks {
            for task in items.iter_mut() {
                if task.get("label").and_then(Value::as_str) == Some(master_name.as_str()) {
                    let renamed = format!("{master_name} (Original)");
                    log::info!("Renamed existing task '{master_name}' to '{renamed}'");
                    if let Value::Object(map) = task {
                        map.insert("label".to_string(), Value::String(renamed));
                    }
                }
            }
            items.push(json!({
                "label": master_name,
                "dependsOn": required,
                "dependsOrder": "parallel",
                "problemMatcher": [],
            }));
            log::info!("Created/updated master task '{master_name}' in tasks.json");
        }
        Ok(merged)
    }
}

pub fn merge_tasks_json(paths: &Paths) -> Result<()> {
    TasksFileMerger { paths }.merge()
}
