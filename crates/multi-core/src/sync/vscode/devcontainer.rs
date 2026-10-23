//! Copy a sub-repo `.devcontainer` folder to the workspace root.

use serde_json::Value;

use crate::error::Result;
use crate::fsx;
use crate::jsonc::{soft_read_json_file, write_json_file};
use crate::paths::Paths;
use crate::repos::load_repos;

use super::helpers::prefix_repo_name_to_path_recursive;

/// Copy the first `.devcontainer` folder found in a sub-repo to the root,
/// overwriting any existing one, and re-point workspace folder paths in
/// `devcontainer.json` at the repo.
pub fn sync_devcontainer(paths: &Paths) -> Result<()> {
    let destination = paths.root_dir.join(".devcontainer");
    for repo in load_repos(paths)? {
        let source = repo.path.join(".devcontainer");
        if !source.is_dir() {
            continue;
        }
        log::info!("Found .devcontainer in {}, copying to root...", repo.name);
        if destination.exists() {
            fsx::remove_dir_all(&destination)?;
        }
        fsx::copy_recursively(&source, &destination)?;

        let devcontainer_json = destination.join("devcontainer.json");
        if devcontainer_json.exists() {
            let parsed = soft_read_json_file(&devcontainer_json);
            if !parsed.is_empty() {
                let prefixed = prefix_repo_name_to_path_recursive(
                    &Value::Object(parsed),
                    &repo.relative_path,
                    true,
                );
                write_json_file(&devcontainer_json, &prefixed, None)?;
                log::debug!("Prefixed paths in devcontainer.json with {}", repo.name);
            }
        }
        log::info!("Successfully copied .devcontainer from {}", repo.name);
        return Ok(());
    }
    log::debug!("No .devcontainer folder found in any repository");
    Ok(())
}
