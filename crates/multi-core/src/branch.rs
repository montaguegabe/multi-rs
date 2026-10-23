//! `multi branch [check]` and `multi set-branch`.

use std::path::Path;

use crate::error::{Error, Result};
use crate::git::{
    check_all_on_same_branch, check_all_repos_are_clean, create_and_switch_branch, describe_head,
    expected_branch_description, expected_branch_for_repo,
};
use crate::paths::Paths;
use crate::repos::load_repos;

/// One line of the branch report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchReportLine {
    pub name: String,
    pub branch: Option<String>,
    pub expected: Option<String>,
    pub is_root: bool,
}

impl BranchReportLine {
    pub fn matches(&self) -> bool {
        self.is_root || (self.branch.is_some() && self.branch == self.expected)
    }
}

/// Collect the branch of the root repo and each sub-repo.
///
/// Read-only: works with dirty trees, mismatched branches, detached HEADs and
/// sub-repos that have not been synced yet (reported with `branch: None`).
pub fn collect_branch_report(paths: &Paths) -> Result<Vec<BranchReportLine>> {
    let root_branch = describe_head(&paths.root_dir)?;
    let mut lines = vec![BranchReportLine {
        name: format!("{} (root)", paths.root_name()),
        branch: Some(root_branch.clone()),
        expected: Some(root_branch.clone()),
        is_root: true,
    }];
    if paths.settings()?.is_monorepo() {
        return Ok(lines);
    }
    for repo in load_repos(paths)? {
        let expected = expected_branch_for_repo(&repo, Some(&root_branch));
        let branch = describe_head(&repo.path).ok();
        let expected = match &branch {
            None => None,
            Some(_) => expected,
        };
        lines.push(BranchReportLine {
            name: repo.name.clone(),
            branch,
            expected: expected.or_else(|| expected_branch_for_repo(&repo, Some(&root_branch))),
            is_root: false,
        });
    }
    Ok(lines)
}

/// Log the branch report. Returns `true` when every repo is present and on
/// its expected branch.
pub fn report_branches(paths: &Paths) -> Result<bool> {
    let root_branch = describe_head(&paths.root_dir)?;
    log::info!("{} (root): {root_branch}", paths.root_name());
    if paths.settings()?.is_monorepo() {
        return Ok(true);
    }
    let mut all_match = true;
    for repo in load_repos(paths)? {
        let branch = match describe_head(&repo.path) {
            Ok(branch) => branch,
            Err(_) => {
                all_match = false;
                log::warn!("{}: (missing — run `multi sync`)", repo.name);
                continue;
            }
        };
        let expected = expected_branch_for_repo(&repo, Some(&root_branch));
        if Some(&branch) == expected.as_ref() {
            log::info!("{}: {branch}", repo.name);
        } else {
            all_match = false;
            let expectation = expected_branch_description(&repo, Some(&root_branch));
            log::warn!("{}: {branch} (expected {expectation})", repo.name);
        }
    }
    Ok(all_match)
}

/// `multi branch check`: report and fail when anything is off.
pub fn check_branch_alignment(paths: &Paths) -> Result<()> {
    if !report_branches(paths)? {
        return Err(Error::Git(
            "Repositories are missing or not on their expected branches. Run `multi sync` to clone missing repos, or `multi set-branch` (with clean working trees) to fix branch mismatches."
                .to_string(),
        ));
    }
    Ok(())
}

/// `multi set-branch`: create/switch `branch_name` in the root and every
/// sub-repo (fixed-branch repos go to their fixed branch).
pub fn set_branch_in_all_repos(root_dir: &Path, branch_name: &str) -> Result<()> {
    let paths = Paths::discover(root_dir, None)?;
    if paths.settings()?.is_monorepo() {
        return Err(Error::Usage(
            "The 'multi set-branch' command is not available in monorepo mode. Use git directly in the root workspace."
                .to_string(),
        ));
    }
    check_all_repos_are_clean(&paths, true)?;
    let all_on_same_branch = check_all_on_same_branch(&paths, false)?;
    if !all_on_same_branch {
        log::warn!(
            "Some repos are not on their expected branches. If the target branches already exist for all repos, this command will fix the situation."
        );
    }
    create_and_switch_branch(&paths.root_dir, branch_name, all_on_same_branch)?;
    for repo in load_repos(&paths)? {
        let repo_branch = expected_branch_for_repo(&repo, Some(branch_name))
            .expect("workspace branch is always set");
        create_and_switch_branch(&repo.path, &repo_branch, all_on_same_branch)?;
    }
    Ok(())
}
