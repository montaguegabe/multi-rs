//! Shared merge machinery for the VS Code config mergers.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::Result;
use crate::fsx;
use crate::jsonc::{
    apply_defaults_to_structure, soft_read_json_file, write_jsonc_file, SOURCE_KEY,
};
use crate::paths::Paths;
use crate::repos::{load_repos, Repository};

use super::GENERATED_HEADER;

const WORKSPACE_FOLDER: &str = "${workspaceFolder}";

/// Point `${workspaceFolder}` at the repo directory. `${workspaceFolder}/..`
/// is normalized to the workspace root.
pub fn prefix_repo_name_to_path(path: &str, repo_name: &str) -> String {
    if path.contains(&format!("{WORKSPACE_FOLDER}/{repo_name}")) {
        return path.to_string();
    }
    let parent = format!("{WORKSPACE_FOLDER}/..");
    if path.contains(&parent) {
        return path.replace(&parent, WORKSPACE_FOLDER);
    }
    path.replace(WORKSPACE_FOLDER, &format!("{WORKSPACE_FOLDER}/{repo_name}"))
}

/// `../x` -> `../<repo>/x`, for files copied from a sub-repo to the root.
pub fn resolve_relative_path(path: &str, repo_name: &str) -> String {
    match path.strip_prefix("../") {
        Some(rest) => format!("../{repo_name}/{rest}"),
        None => path.to_string(),
    }
}

/// Recursively adjust workspace folder paths (and optionally `../` paths)
/// inside any JSON value.
pub fn prefix_repo_name_to_path_recursive(
    value: &Value,
    repo_name: &str,
    resolve_relative_paths: bool,
) -> Value {
    match value {
        Value::String(text) => {
            let mut result = text.clone();
            if result.contains(WORKSPACE_FOLDER) {
                result = prefix_repo_name_to_path(&result, repo_name);
            }
            if resolve_relative_paths && result.starts_with("../") {
                result = resolve_relative_path(&result, repo_name);
            }
            Value::String(result)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        prefix_repo_name_to_path_recursive(v, repo_name, resolve_relative_paths),
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| {
                    prefix_repo_name_to_path_recursive(item, repo_name, resolve_relative_paths)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn deep_merge_recursive(
    base: &Map<String, Value>,
    override_map: &Map<String, Value>,
    skip_keys: Option<&[String]>,
) -> Map<String, Value> {
    let mut merged = base.clone();
    for (key, value) in override_map {
        if let Some(skip) = skip_keys {
            if skip.iter().any(|s| s == key) {
                continue;
            }
        }
        match (merged.get(key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                let next = deep_merge_recursive(existing, incoming, skip_keys);
                merged.insert(key.clone(), Value::Object(next));
            }
            (Some(Value::Array(existing)), Value::Array(incoming)) => {
                // Concatenate, dropping duplicates while preserving order.
                let mut next = existing.clone();
                for item in incoming {
                    if !next.contains(item) {
                        next.push(item.clone());
                    }
                }
                merged.insert(key.clone(), Value::Array(next));
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

/// Deep-merge `override_map` into `base`. When `repo_name` is given, paths in
/// the override are first re-pointed at that repo directory.
pub fn deep_merge(
    base: &Map<String, Value>,
    override_map: &Map<String, Value>,
    repo_name: Option<&str>,
    skip_keys: Option<&[String]>,
) -> Map<String, Value> {
    let effective = match repo_name {
        Some(name) => match prefix_repo_name_to_path_recursive(
            &Value::Object(override_map.clone()),
            name,
            false,
        ) {
            Value::Object(map) => map,
            _ => unreachable!(),
        },
        None => override_map.clone(),
    };
    deep_merge_recursive(base, &effective, skip_keys)
}

/// Behavior shared by every `.vscode/*.json` merger.
pub trait VscodeFileMerger {
    fn paths(&self) -> &Paths;
    fn destination_json_path(&self) -> PathBuf;
    fn source_json_path(&self, repo_path: &Path) -> PathBuf;

    fn repo_defaults(&self, _repo: &Repository) -> Option<Value> {
        None
    }

    fn skip_keys(&self, _repo: &Repository) -> Option<Vec<String>> {
        None
    }

    fn source_tracked_keys(&self) -> &'static [&'static str] {
        &["tasks", "configurations", "compounds", "inputs"]
    }

    fn add_source_tracking(
        &self,
        repo_json: &Map<String, Value>,
        source_file_display: &str,
    ) -> Map<String, Value> {
        let mut result = repo_json.clone();
        for key in self.source_tracked_keys() {
            if let Some(Value::Array(items)) = result.get(*key) {
                let tracked: Vec<Value> = items
                    .iter()
                    .map(|item| match item {
                        Value::Object(map) => {
                            let mut next = map.clone();
                            next.insert(
                                SOURCE_KEY.to_string(),
                                Value::String(source_file_display.to_string()),
                            );
                            Value::Object(next)
                        }
                        other => other.clone(),
                    })
                    .collect();
                result.insert(key.to_string(), Value::Array(tracked));
            }
        }
        result
    }

    /// Hook for subclasses to adjust `repo_json` before the generic merge.
    fn prepare_repo_json(
        &self,
        repo_json: Map<String, Value>,
        _repo: &Repository,
    ) -> Result<Map<String, Value>> {
        Ok(repo_json)
    }

    fn merge_repo_json(
        &self,
        merged: &Map<String, Value>,
        repo_json: Map<String, Value>,
        repo: &Repository,
        source_json_path: &Path,
    ) -> Result<Map<String, Value>> {
        let repo_json = self.prepare_repo_json(repo_json, repo)?;
        let skip_keys = self.skip_keys(repo);
        let effective = match self.repo_defaults(repo) {
            Some(defaults) => {
                match apply_defaults_to_structure(Some(&Value::Object(repo_json)), &defaults) {
                    Value::Object(map) => map,
                    _ => Map::new(),
                }
            }
            None => repo_json,
        };
        let source_display = fsx::resolve(source_json_path).display().to_string();
        let effective = self.add_source_tracking(&effective, &source_display);
        Ok(deep_merge(
            merged,
            &effective,
            Some(&repo.relative_path),
            skip_keys.as_deref(),
        ))
    }

    fn post_process_json(&self, merged: Map<String, Value>) -> Result<Map<String, Value>> {
        Ok(merged)
    }

    /// Merge the source file of every (non-skipped) repo into the destination.
    fn merge(&self) -> Result<()> {
        let destination = self.destination_json_path();
        if destination.exists() {
            fsx::remove_file(&destination)?;
        }
        let mut merged = Map::new();
        for repo in load_repos(self.paths())? {
            if repo.skip_vscode {
                log::debug!(
                    "Skipping {} for {}",
                    repo.name,
                    fsx::file_name(&destination)
                );
                continue;
            }
            let source_path = self.source_json_path(&repo.path);
            let repo_json = soft_read_json_file(&source_path);
            merged = self.merge_repo_json(&merged, repo_json, &repo, &source_path)?;
        }
        let merged = self.post_process_json(merged)?;
        write_jsonc_file(&destination, &Value::Object(merged), Some(GENERATED_HEADER))?;
        log::info!(
            "Successfully merged files into {}",
            fsx::file_name(&destination)
        );
        Ok(())
    }
}

/// Mark list items whose `name_key` is in `names` as `"required": true`.
pub(super) fn mark_required(items: Option<&mut Value>, name_key: &str, names: &[String]) {
    if names.is_empty() {
        return;
    }
    if let Some(Value::Array(items)) = items {
        for item in items.iter_mut() {
            if let Value::Object(map) = item {
                let matches = map
                    .get(name_key)
                    .and_then(Value::as_str)
                    .map(|name| names.iter().any(|n| n == name))
                    .unwrap_or(false);
                if matches {
                    map.insert("required".to_string(), Value::Bool(true));
                }
            }
        }
    }
}

/// Deep-merge a workspace-level shared file (if present) into `merged`.
pub(super) fn merge_shared_file(
    merged: Map<String, Value>,
    shared_path: &Path,
    label: &str,
) -> Map<String, Value> {
    if !shared_path.exists() {
        log::debug!(
            "Shared {label} file not found at {}, skipping.",
            fsx::file_name(shared_path)
        );
        return merged;
    }
    let shared = soft_read_json_file(shared_path);
    if shared.is_empty() {
        log::debug!(
            "Shared {label} file at {} is empty or invalid, skipping.",
            fsx::file_name(shared_path)
        );
        return merged;
    }
    log::info!(
        "Merged shared {label} config from {}",
        fsx::file_name(shared_path)
    );
    deep_merge(&merged, &shared, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefixes_workspace_folder() {
        assert_eq!(
            prefix_repo_name_to_path("${workspaceFolder}/x", "api"),
            "${workspaceFolder}/api/x"
        );
        assert_eq!(
            prefix_repo_name_to_path("${workspaceFolder}/api/x", "api"),
            "${workspaceFolder}/api/x"
        );
        assert_eq!(
            prefix_repo_name_to_path("${workspaceFolder}/../x", "api"),
            "${workspaceFolder}/x"
        );
        assert_eq!(
            resolve_relative_path("../docker-compose.yml", "api"),
            "../api/docker-compose.yml"
        );
    }

    #[test]
    fn merges_deeply() {
        let base = json!({"a": {"x": 1}, "list": [1, 2]})
            .as_object()
            .unwrap()
            .clone();
        let over = json!({"a": {"y": 2}, "list": [2, 3], "skip": 1, "p": "${workspaceFolder}/f"})
            .as_object()
            .unwrap()
            .clone();
        let merged = deep_merge(&base, &over, Some("repo"), Some(&["skip".to_string()]));
        assert_eq!(merged["a"], json!({"x": 1, "y": 2}));
        assert_eq!(merged["list"], json!([1, 2, 3]));
        assert!(!merged.contains_key("skip"));
        assert_eq!(merged["p"], "${workspaceFolder}/repo/f");
    }
}
