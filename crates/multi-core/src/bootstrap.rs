//! Root repository and README bootstrap performed by `multi sync`.

use std::path::Path;

use crate::error::Result;
use crate::fsx;
use crate::git::{init_repo, is_git_repo_root};
use crate::paths::Paths;
use crate::repos::load_repos;

const INIT_README_TEMPLATE: &str = include_str!("../resources/init_readme.md");

fn render_init_readme(workspace_name: &str, repo_list: &str) -> String {
    INIT_README_TEMPLATE
        .replace("{**name**}", workspace_name)
        .replace("{**repo_list**}", repo_list)
}

fn to_https_repo_link(url: &str) -> String {
    if let Some(slug) = url.strip_prefix("git@github.com:") {
        return format!("https://github.com/{}", slug.replace(".git", ""));
    }
    if let Some(slug) = url.strip_prefix("ssh://git@github.com/") {
        return format!("https://github.com/{}", slug.replace(".git", ""));
    }
    url.replace(".git", "")
}

pub fn build_repo_list(paths: &Paths) -> Result<String> {
    let mut entries = Vec::new();
    for repo in load_repos(paths)? {
        match &repo.url {
            Some(url) => entries.push(format!(
                "- [{}]({})",
                repo.name.replace(".git", ""),
                to_https_repo_link(url)
            )),
            None => entries.push(format!("- `{}`", repo.name)),
        }
    }
    Ok(entries.join("\n"))
}

pub fn ensure_root_git_repo(root_dir: &Path) -> Result<()> {
    if is_git_repo_root(root_dir) {
        return Ok(());
    }
    log::info!("Initializing git repository at workspace root...");
    init_repo(root_dir)
}

pub fn ensure_workspace_readme(paths: &Paths) -> Result<()> {
    let readme_path = paths.root_dir.join("README.md");
    if readme_path.exists() {
        return Ok(());
    }
    let repo_list = build_repo_list(paths)?;
    let content = render_init_readme(&paths.root_name(), &repo_list);
    fsx::write(&readme_path, content)?;
    log::info!("Created README.md");
    Ok(())
}
