//! `multi.json` settings with defaults applied.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::fsx;
use crate::jsonc::apply_defaults_to_structure;

pub const DEFAULT_AGENTS_PARTS_DIR: &str = "AGENTS.parts";

fn default_settings() -> Value {
    json!({
        "monoRepo": false,
        "allowSymlinks": false,
        "agentInstructions": {
            "enabled": false,
            "partsDir": DEFAULT_AGENTS_PARTS_DIR,
            "includeRepoDescriptions": true,
        },
        "vscode": {"skipSettings": ["workbench.colorCustomizations"]},
        "repos": [],
    })
}

/// Parsed `multi.json` with defaults filled in for missing keys.
#[derive(Debug, Clone)]
pub struct Settings {
    value: Map<String, Value>,
}

/// Agent instruction generation options (`agentInstructions`).
#[derive(Debug, Clone)]
pub struct AgentInstructions {
    pub enabled: bool,
    pub parts_dir: String,
    pub include_repo_descriptions: bool,
}

impl Settings {
    pub fn from_multi_json_file(path: &Path) -> Result<Self> {
        let text = fsx::read_to_string(path)?;
        Self::from_text(&text)
    }

    pub fn from_text(text: &str) -> Result<Self> {
        let user: Value = serde_json::from_str(text)?;
        Self::from_value(user)
    }

    pub fn from_value(user: Value) -> Result<Self> {
        if !user.is_object() {
            return Err(Error::Config(
                "multi.json must contain a top-level object.".to_string(),
            ));
        }
        match apply_defaults_to_structure(Some(&user), &default_settings()) {
            Value::Object(map) => Ok(Settings { value: map }),
            _ => unreachable!("defaults are an object"),
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.value.get(key)
    }

    pub fn as_map(&self) -> &Map<String, Value> {
        &self.value
    }

    pub fn is_monorepo(&self) -> bool {
        self.get("monoRepo")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub fn allow_symlinks(&self) -> bool {
        self.get("allowSymlinks")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Raw repo configs (`repos`), empty when absent or not a list.
    pub fn repo_configs(&self) -> Vec<Value> {
        self.get("repos")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    pub fn has_repos(&self) -> bool {
        !self.repo_configs().is_empty()
    }

    pub fn vscode_skip_settings(&self) -> Vec<String> {
        self.get("vscode")
            .and_then(|v| v.get("skipSettings"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether a feature section is on: `"<key>": {"enabled": bool}` or the
    /// shorthand `"<key>": bool`, defaulting to `default` when absent.
    pub fn section_enabled(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some(Value::Object(section)) => section.get("enabled").map(truthy).unwrap_or(default),
            Some(Value::Null) | None => default,
            Some(value) => truthy(value),
        }
    }

    pub fn agent_instructions(&self) -> AgentInstructions {
        let section = self
            .get("agentInstructions")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let parts_dir = section
            .get("partsDir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_AGENTS_PARTS_DIR)
            .to_string();
        AgentInstructions {
            enabled: self.section_enabled("agentInstructions", false),
            parts_dir,
            include_repo_descriptions: section
                .get("includeRepoDescriptions")
                .map(truthy)
                .unwrap_or(true),
        }
    }

    /// Workspace-relative git hooks directory (`hooks.path`), validated.
    pub fn hooks_path(&self) -> Result<Option<String>> {
        let Some(hooks) = self.get("hooks") else {
            return Ok(None);
        };
        if hooks.is_null() {
            return Ok(None);
        }
        let Value::Object(map) = hooks else {
            return Err(Error::Config(
                "multi.json field \"hooks\" must be an object.".to_string(),
            ));
        };
        let Some(raw) = map.get("path") else {
            return Ok(None);
        };
        if raw.is_null() {
            return Ok(None);
        }
        let raw = raw
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::Config(
                    "multi.json field \"hooks.path\" must be a non-empty string.".to_string(),
                )
            })?;
        let normalized = normalize_relative_posix(raw).ok_or_else(|| {
            Error::Config(
                "multi.json field \"hooks.path\" must be a relative path inside the workspace (no leading '/' and no '..')."
                    .to_string(),
            )
        })?;
        Ok(Some(normalized))
    }

    /// `worktree.symlink` / `worktree.copy` entries.
    pub fn worktree_transfer_paths(&self, transfer_type: &str) -> Result<Vec<String>> {
        let Some(section) = self.get("worktree") else {
            return Ok(Vec::new());
        };
        let Value::Object(map) = section else {
            return Err(Error::Git(
                "multi.json field 'worktree' must be an object.".to_string(),
            ));
        };
        let Some(entries) = map.get(transfer_type) else {
            return Ok(Vec::new());
        };
        if entries.is_null() {
            return Ok(Vec::new());
        }
        let items = entries
            .as_array()
            .ok_or_else(|| string_array_error(transfer_type))?;
        items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| string_array_error(transfer_type))
            })
            .collect()
    }
}

fn string_array_error(transfer_type: &str) -> Error {
    Error::Git(format!(
        "multi.json field 'worktree.{transfer_type}' must be a string array."
    ))
}

/// Python-style truthiness for JSON values.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Normalize a POSIX relative path (`a//b/./c` -> `a/b/c`). Returns `None`
/// for empty, absolute, or parent-escaping paths.
pub fn normalize_relative_posix(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('/') {
        return None;
    }
    let parts: Vec<&str> = raw
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.contains(&"..") {
        return None;
    }
    if parts.is_empty() {
        // Python's PurePosixPath(".") renders as "."; callers never need it.
        return Some(".".to_string());
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_applied() {
        let settings = Settings::from_text(r#"{"repos": [{"url": "https://x/y"}]}"#).unwrap();
        assert!(!settings.is_monorepo());
        assert_eq!(
            settings.vscode_skip_settings(),
            vec!["workbench.colorCustomizations".to_string()]
        );
        let agents = settings.agent_instructions();
        assert!(!agents.enabled);
        assert_eq!(agents.parts_dir, "AGENTS.parts");
        assert!(agents.include_repo_descriptions);
        assert_eq!(settings.hooks_path().unwrap(), None);
    }

    #[test]
    fn hooks_path_validation() {
        let ok = Settings::from_text(r#"{"hooks": {"path": "./.githooks/"}}"#).unwrap();
        assert_eq!(ok.hooks_path().unwrap(), Some(".githooks".to_string()));
        for bad in ["/abs", "../up", "", " "] {
            let text = format!(r#"{{"hooks": {{"path": "{bad}"}}}}"#);
            let settings = Settings::from_text(&text).unwrap();
            assert!(settings.hooks_path().is_err(), "{bad:?} should be rejected");
        }
    }
}
