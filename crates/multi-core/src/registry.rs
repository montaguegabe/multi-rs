//! Global repository registry (`~/.multi/repos.json`) used for symlinking
//! existing clones instead of cloning again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::fsx;
use crate::repo_urls::normalize_repo_url;

const REGISTRY_VERSION: u64 = 1;

pub fn registry_file() -> PathBuf {
    fsx::multi_home_dir().join("repos.json")
}

/// Map of normalized URL -> absolute path.
pub fn load_registry() -> BTreeMap<String, String> {
    load_registry_from(&registry_file())
}

fn load_registry_from(path: &Path) -> BTreeMap<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(err) => {
            log::warn!("Failed to load registry: {err}");
            return BTreeMap::new();
        }
    };
    let data: Value = match serde_json::from_str(&text) {
        Ok(data) => data,
        Err(err) => {
            log::warn!("Failed to load registry: {err}");
            return BTreeMap::new();
        }
    };
    let Value::Object(map) = data else {
        log::warn!("Invalid registry format, resetting");
        return BTreeMap::new();
    };
    let repos = if map.contains_key("version") && map.contains_key("repos") {
        map.get("repos")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    } else {
        map
    };
    repos
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect()
}

pub fn save_registry(repos: &BTreeMap<String, String>) {
    save_registry_to(&registry_file(), repos)
}

fn save_registry_to(path: &Path, repos: &BTreeMap<String, String>) {
    let data = json!({"version": REGISTRY_VERSION, "repos": repos});
    let text = serde_json::to_string_pretty(&data).unwrap_or_default();
    if let Err(err) = fsx::write(path, text) {
        log::warn!("Failed to save registry: {err}");
    }
}

/// Remember where `url` is cloned.
pub fn register_repo(url: &str, path: &Path) {
    let mut repos = load_registry();
    let normalized = normalize_repo_url(url);
    repos.insert(normalized.clone(), fsx::resolve(path).display().to_string());
    save_registry(&repos);
    log::debug!("Registered repo {normalized} at {}", path.display());
}

/// Known clone of `url`, when it still exists and is a git repository.
pub fn lookup_repo(url: &str) -> Option<PathBuf> {
    let repos = load_registry();
    let normalized = normalize_repo_url(url);
    let path = PathBuf::from(repos.get(&normalized)?);
    if !path.exists() {
        log::debug!("Registry entry for {normalized} points to non-existent path");
        return None;
    }
    if !path.join(".git").exists() {
        log::debug!("Registry entry for {normalized} is not a git repository");
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_reads_legacy_format() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("repos.json");
        std::fs::write(&file, r#"{"https://x/y": "/tmp/y"}"#).unwrap();
        assert_eq!(
            load_registry_from(&file).get("https://x/y").unwrap(),
            "/tmp/y"
        );

        let mut repos = BTreeMap::new();
        repos.insert("https://x/z".to_string(), "/tmp/z".to_string());
        save_registry_to(&file, &repos);
        assert_eq!(load_registry_from(&file), repos);
    }
}
