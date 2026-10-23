//! Merge `extensions.json` recommendations.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::Result;
use crate::paths::{vscode_config_dir, Paths};

use super::helpers::VscodeFileMerger;

struct ExtensionsFileMerger<'a> {
    paths: &'a Paths,
}

impl VscodeFileMerger for ExtensionsFileMerger<'_> {
    fn paths(&self) -> &Paths {
        self.paths
    }

    fn destination_json_path(&self) -> PathBuf {
        self.paths.vscode_extensions_path()
    }

    fn source_json_path(&self, repo_path: &Path) -> PathBuf {
        vscode_config_dir(repo_path).join("extensions.json")
    }

    fn post_process_json(&self, mut merged: Map<String, Value>) -> Result<Map<String, Value>> {
        let recommendations = merged
            .remove("recommendations")
            .and_then(|v| match v {
                Value::Array(items) => Some(items),
                _ => None,
            })
            .unwrap_or_default();
        let mut unique: Vec<Value> = Vec::new();
        for item in recommendations {
            if !unique.contains(&item) {
                unique.push(item);
            }
        }
        merged.insert("recommendations".to_string(), Value::Array(unique));
        Ok(merged)
    }
}

pub fn merge_extensions_json(paths: &Paths) -> Result<()> {
    ExtensionsFileMerger { paths }.merge()
}
