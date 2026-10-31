mod common;

use common::*;
use multi_core::git::is_git_repo_root;
use multi_core::worktree::{add_worktree, list_linked_worktree_paths, WorktreeAddOptions};
use serde_json::json;

fn remote_workspace(
    branches: &[&str],
    fixed: bool,
    worktree_config: Option<serde_json::Value>,
) -> (Workspace, std::path::PathBuf) {
    setup();
    let scratch = tempfile::tempdir().unwrap();
    // Keep remotes alive for the duration of the test by leaking the dir.
    let scratch_path = scratch.keep();
    let remote = create_remote(&scratch_path, "repo0", branches);
    let mut repo = json!({"url": remote.to_str().unwrap(), "name": "repo0"});
    if fixed {
        repo["fixedBranch"] = json!("main");
    }
    let extra = match worktree_config {
        Some(config) => json!({"worktree": config}),
        None => json!({}),
    };
    (workspace_with_remotes(&[repo], extra), scratch_path)
}

#[test]
fn add_worktree_uses_name_as_default_branch_in_sibling_directory() {
    let (ws, scratch) = remote_workspace(&[], false, None);
    assert!(!ws.tmp.path().join("workspace-worktrees").exists());

    let destination =
        add_worktree(&ws.root, "feature-test", &WorktreeAddOptions::default()).unwrap();

    assert_eq!(
        destination,
        ws.tmp.path().join("workspace-worktrees/feature-test")
    );
    assert!(is_git_repo_root(&destination));
    assert_eq!(current_branch(&destination), "feature-test");
    assert_eq!(current_branch(&destination.join("repo0")), "feature-test");
    // `git worktree list` reports canonical paths (on macOS the temp dir
    // lives under `/private/var`).
    assert_eq!(
        list_linked_worktree_paths(&ws.root),
        vec![std::fs::canonicalize(&destination)
            .unwrap()
            .display()
            .to_string()]
    );
    assert!(list_linked_worktree_paths(&destination).is_empty());
    let _ = std::fs::remove_dir_all(scratch);
}

#[test]
fn add_worktree_allows_dirty_trees_custom_branch_and_fixed_branch() {
    let (ws, scratch) = remote_workspace(&["feature/user-auth"], true, None);
    write(&ws.root.join("README.md"), "# edited\n");
    write(&ws.root.join("untracked.txt"), "x");
    write(&ws.repo(0).join("README.md"), "# edited repo\n");

    let destination = add_worktree(
        &ws.root,
        "user-auth",
        &WorktreeAddOptions {
            branch_name: Some("feature/user-auth".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(current_branch(&destination), "feature/user-auth");
    // Fixed-branch repo stays on main even though the remote has the branch.
    assert_eq!(current_branch(&destination.join("repo0")), "main");
    // Source trees untouched.
    assert_eq!(read(&ws.root.join("README.md")), "# edited\n");

    assert!(
        add_worktree(&ws.root, "user-auth", &WorktreeAddOptions::default()).is_err(),
        "destination exists"
    );
    assert!(
        add_worktree(&ws.root, "a/b", &WorktreeAddOptions::default()).is_err(),
        "name must be single component"
    );
    let _ = std::fs::remove_dir_all(scratch);
}

#[test]
fn add_worktree_transfers_gitignored_paths() {
    let (ws, scratch) = remote_workspace(
        &[],
        false,
        Some(json!({"symlink": [".env"], "copy": [".cache", "repo0/.secret"]})),
    );
    write(&ws.root.join(".env"), "SECRET=1\n");
    write(&ws.root.join(".cache/data.txt"), "cached\n");
    write(&ws.repo(0).join(".secret"), "repo secret\n");

    let destination =
        add_worktree(&ws.root, "feature-files", &WorktreeAddOptions::default()).unwrap();
    assert!(std::fs::symlink_metadata(destination.join(".env"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(read(&destination.join(".env")), "SECRET=1\n");
    assert_eq!(read(&destination.join(".cache/data.txt")), "cached\n");
    assert!(!std::fs::symlink_metadata(destination.join(".cache"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(read(&destination.join("repo0/.secret")), "repo secret\n");

    // A tracked path is refused.
    ws.update_multi_json(|config| {
        config.insert("worktree".into(), json!({"copy": ["README.md"]}));
    });
    let err = add_worktree(&ws.root, "feature-bad", &WorktreeAddOptions::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("not gitignored"), "{err}");
    let _ = std::fs::remove_dir_all(scratch);
}

#[test]
fn add_worktree_uses_base_ref_for_root_and_subrepos() {
    let (ws, scratch) = remote_workspace(&[], false, None);
    write(&ws.root.join("new.txt"), "new");
    commit_all(&ws.root, "Second root commit");
    let base_root = git(&ws.root, &["rev-parse", "HEAD~1"]);
    write(&ws.repo(0).join("new.txt"), "new");
    commit_all(ws.repo(0), "Second repo commit");
    git(ws.repo(0), &["tag", "base-tag", "HEAD~1"]);
    git(&ws.root, &["tag", "base-tag", &base_root]);

    let destination = add_worktree(
        &ws.root,
        "from-base",
        &WorktreeAddOptions {
            base_ref: Some("base-tag".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(git(&destination, &["rev-parse", "HEAD"]), base_root);
    assert!(!destination.join("new.txt").exists());
    assert_eq!(
        git(&destination.join("repo0"), &["rev-parse", "HEAD"]),
        git(ws.repo(0), &["rev-parse", "base-tag"])
    );
    let _ = std::fs::remove_dir_all(scratch);
}

#[test]
fn add_worktree_respects_install_set() {
    setup();
    let scratch = tempfile::tempdir().unwrap().keep();
    let remote_a = create_remote(&scratch, "public", &[]);
    let remote_b = create_remote(&scratch, "private", &[]);
    let ws = workspace_with_remotes(
        &[
            json!({"url": remote_a.to_str().unwrap(), "name": "public", "installSets": ["default"]}),
            json!({"url": remote_b.to_str().unwrap(), "name": "private", "installSets": ["dev"]}),
        ],
        json!({}),
    );
    let destination = add_worktree(
        &ws.root,
        "subset",
        &WorktreeAddOptions {
            install_set: Some("default".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(destination.join("public/.git").exists());
    assert!(!destination.join("private").exists());
    let _ = std::fs::remove_dir_all(scratch);
}
