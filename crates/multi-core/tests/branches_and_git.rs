mod common;

use common::*;
use multi_core::branch::{
    check_branch_alignment, collect_branch_report, report_branches, set_branch_in_all_repos,
};
use multi_core::git::{
    check_all_on_same_branch, describe_head, get_current_branch, is_git_repo_root,
};
use multi_core::git_run::run_git_in_all_repos;
use multi_core::Error;
use serde_json::json;

#[test]
fn report_branches_works_with_dirty_trees_and_flags_mismatches() {
    let ws = workspace();
    write(&ws.repo(0).join("dirty.txt"), "dirty");
    assert!(report_branches(&ws.paths()).unwrap());

    git(ws.repo(1), &["checkout", "-q", "-b", "other"]);
    assert!(!report_branches(&ws.paths()).unwrap());
    assert!(matches!(
        check_branch_alignment(&ws.paths()),
        Err(Error::Git(_))
    ));

    let report = collect_branch_report(&ws.paths()).unwrap();
    assert_eq!(report[0].name, "root (root)");
    assert_eq!(report[2].branch.as_deref(), Some("other"));
    assert_eq!(report[2].expected.as_deref(), Some("main"));
    assert!(!report[2].matches());
}

#[test]
fn report_branches_respects_fixed_branch() {
    let ws = workspace();
    ws.update_multi_json(|config| {
        config["repos"][1]["fixedBranch"] = json!("stable");
    });
    git(ws.repo(1), &["checkout", "-q", "-b", "stable"]);
    assert!(report_branches(&ws.paths()).unwrap());
    assert!(check_all_on_same_branch(&ws.paths(), true).unwrap());
    // Running git across repos accepts the fixed-branch state.
    run_git_in_all_repos(&ws.paths(), &["status".to_string(), "--short".to_string()]).unwrap();
}

#[test]
fn detached_head_is_described_and_never_matches() {
    let ws = workspace();
    let sha = git(ws.repo(0), &["rev-parse", "--short", "HEAD"]);
    git(ws.repo(0), &["checkout", "-q", "--detach"]);
    assert_eq!(
        describe_head(ws.repo(0)).unwrap(),
        format!("(detached at {sha})")
    );
    assert_eq!(get_current_branch(ws.repo(0)).unwrap(), "HEAD");
    assert!(!report_branches(&ws.paths()).unwrap());
    let err = check_all_on_same_branch(&ws.paths(), true)
        .unwrap_err()
        .to_string();
    assert!(err.contains("(detached at"), "{err}");
}

#[test]
fn report_continues_past_missing_and_non_git_repos() {
    let ws = workspace();
    std::fs::remove_dir_all(ws.repo(1)).unwrap();
    assert!(!report_branches(&ws.paths()).unwrap());
    let report = collect_branch_report(&ws.paths()).unwrap();
    assert_eq!(report.len(), 3);
    assert_eq!(report[2].branch, None);

    std::fs::create_dir_all(ws.repo(1)).unwrap();
    assert!(!report_branches(&ws.paths()).unwrap());
    let err = get_current_branch(ws.repo(1)).unwrap_err().to_string();
    assert!(err.contains("not a git repository"), "{err}");
    assert!(err.contains("Run `multi sync`"), "{err}");

    // Missing repos are skipped by the same-branch check (install sets).
    std::fs::remove_dir_all(ws.repo(1)).unwrap();
    assert!(check_all_on_same_branch(&ws.paths(), true).unwrap());
}

#[test]
fn run_git_in_all_repos_requires_expected_branches_but_allows_dirty_trees() {
    let ws = workspace();
    write(&ws.root.join("dirty.txt"), "dirty");
    run_git_in_all_repos(
        &ws.paths(),
        &["branch".to_string(), "--show-current".to_string()],
    )
    .unwrap();
    git(ws.repo(0), &["checkout", "-q", "-b", "feature"]);
    assert!(matches!(
        run_git_in_all_repos(&ws.paths(), &["status".to_string()]),
        Err(Error::Git(_))
    ));
}

#[test]
fn set_branch_creates_and_switches_everywhere_except_fixed_repos() {
    let ws = workspace();
    ws.update_multi_json(|config| {
        config["repos"][1]["fixedBranch"] = json!("main");
    });
    commit_all(&ws.root, "config");
    set_branch_in_all_repos(&ws.root, "feature/x").unwrap();
    assert_eq!(current_branch(&ws.root), "feature/x");
    assert_eq!(current_branch(ws.repo(0)), "feature/x");
    assert_eq!(current_branch(ws.repo(1)), "main");

    // Idempotent and switches back to existing branches.
    set_branch_in_all_repos(&ws.root, "main").unwrap();
    assert_eq!(current_branch(ws.repo(0)), "main");

    // Dirty trees block set-branch.
    write(&ws.repo(0).join("dirty.txt"), "x");
    assert!(matches!(
        set_branch_in_all_repos(&ws.root, "feature/y"),
        Err(Error::RepoNotClean(_))
    ));
}

#[test]
fn is_git_repo_root_distinguishes_nested_directories() {
    let ws = workspace();
    assert!(is_git_repo_root(&ws.root));
    assert!(is_git_repo_root(ws.repo(0)));
    let nested = ws.root.join("not-a-repo");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(!is_git_repo_root(&nested));
}
