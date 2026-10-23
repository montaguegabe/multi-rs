mod common;

use common::*;
use multi_core::app_api::{
    get_combined_history, get_history_group_diff, get_project_branches, get_project_detail,
    get_project_status, get_project_summary, get_projects_summary, get_repo_diff, get_repo_status,
    get_repo_sync_state, list_project_repo_names, refresh_projects_summary,
    resolve_navigation_path, HistoryCommitRef, RepoStatus,
};
use serde_json::json;

#[test]
fn statuses_and_sync_states() {
    let ws = workspace();
    assert_eq!(get_repo_status(ws.repo(0)), RepoStatus::Clean);
    assert_eq!(get_project_status(&ws.root), RepoStatus::Clean);
    assert_eq!(get_repo_status(&ws.root.join("nope")), RepoStatus::Missing);
    let plain = ws.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(get_repo_status(&plain), RepoStatus::NoGit);
    assert!(!get_repo_sync_state(&plain).is_repo);

    write(&ws.repo(1).join("dirty.txt"), "x");
    assert_eq!(get_repo_status(ws.repo(1)), RepoStatus::Dirty);
    assert_eq!(get_project_status(&ws.root), RepoStatus::Dirty);
    // Root untracked files are ignored for the project status.
    std::fs::remove_file(ws.repo(1).join("dirty.txt")).unwrap();
    write(&ws.root.join("untracked.txt"), "x");
    assert_eq!(get_project_status(&ws.root), RepoStatus::Clean);

    write(&ws.repo(0).join("c.txt"), "x");
    commit_all(ws.repo(0), "ahead");
    assert_eq!(get_repo_status(ws.repo(0)), RepoStatus::OutOfSync);
    let state = get_repo_sync_state(ws.repo(0));
    assert_eq!(
        (state.ahead, state.behind, state.has_upstream, state.is_repo),
        (1, 0, true, true)
    );
    assert_eq!(get_project_status(&ws.root), RepoStatus::OutOfSync);
}

#[test]
fn branches_subrepos_and_summary() {
    let ws = workspace();
    assert_eq!(list_project_repo_names(&ws.root), vec!["repo0", "repo1"]);
    assert!(list_project_repo_names(ws.repo(0)).is_empty());
    let branches = get_project_branches(&ws.root);
    assert_eq!(branches.len(), 3);
    assert_eq!(branches[0].name, "root");
    assert!(branches.iter().all(|b| b.branch == "main"));

    let summary = get_project_summary(&ws.root);
    assert_eq!(summary.name, "root");
    assert_eq!(summary.status, RepoStatus::Clean);
    assert!(summary.last_commit_ms.is_some());
    assert!(summary.last_modified_ms.is_some());
    assert_eq!(summary.sub_repos.as_ref().unwrap().len(), 2);
    assert!(summary.doctor_result.is_some());
    let json = serde_json::to_value(&summary).unwrap();
    assert_eq!(json["status"], "clean");
    assert!(json.get("lastCommitMs").is_some());
    assert!(json.get("worktrees").is_none());

    let paths = vec![
        ws.root.display().to_string(),
        ws.repo(0).display().to_string(),
    ];
    let fresh = refresh_projects_summary(&paths);
    assert_eq!(fresh.len(), 2);
    assert_eq!(fresh[1].status, RepoStatus::Clean);
    assert!(fresh[1].sub_repos.is_none());
    let cached = get_projects_summary(&paths);
    assert_eq!(
        cached[0].status,
        RepoStatus::Clean,
        "served from the refreshed cache"
    );
}

#[test]
fn diffs_history_and_detail() {
    let ws = workspace();
    write(&ws.repo(0).join("README.md"), "# changed\n");
    write(&ws.repo(0).join("new.txt"), "brand new\n");
    let diff = get_repo_diff(ws.repo(0));
    assert!(diff.contains("-# Sub Repository repo0"));
    assert!(diff.contains("+brand new"));
    assert!(diff.contains("b/new.txt"));

    write(&ws.repo(1).join("later.txt"), "x");
    commit_all_at(ws.repo(1), "Later commit\n\nWith a body", 4_000_000_000);
    let groups = get_combined_history(&ws.root);
    assert!(!groups.is_empty());
    let newest = &groups[0].commits[0];
    assert_eq!(newest.subject, "Later commit");
    assert_eq!(newest.description, "With a body");
    assert_eq!(newest.repo_name, "repo1");
    assert!(groups
        .iter()
        .flat_map(|g| g.repo_names.iter())
        .any(|n| n == "root"));

    let diffs = get_history_group_diff(&[HistoryCommitRef {
        repo_path: ws.repo(1).display().to_string(),
        repo_name: "repo1".into(),
        hash: newest.hash.clone(),
    }]);
    assert_eq!(diffs.len(), 1);
    assert!(diffs[0].diff.contains("later.txt"));

    let detail = get_project_detail(&ws.root);
    assert_eq!(detail.repositories.len(), 3);
    assert_eq!(detail.repositories[1].name, "repo0");
    assert!(detail.repositories[1].diff.contains("brand new"));
    assert_eq!(detail.sync_states_by_repo_path.len(), 3);
    assert!(detail.sync_states_by_repo_path[&ws.repo(1).display().to_string()].ahead >= 1);
    let value = serde_json::to_value(&detail).unwrap();
    assert!(value["historyGroups"].is_array());
    assert!(value["syncStatesByRepoPath"].is_object());
    assert_eq!(value["repoPath"], json!(ws.root.display().to_string()));
}

#[test]
fn navigation_path_resolution() {
    let ws = workspace();
    let (repo, file) = resolve_navigation_path(&ws.repo(0).join("README.md")).unwrap();
    assert_eq!(repo, std::fs::canonicalize(ws.repo(0)).unwrap());
    assert_eq!(file.as_deref(), Some("README.md"));
    let (dir, file) = resolve_navigation_path(ws.repo(1)).unwrap();
    assert_eq!(dir, std::fs::canonicalize(ws.repo(1)).unwrap());
    assert!(file.is_none());
    assert!(resolve_navigation_path(&ws.root.join("missing")).is_none());
}
