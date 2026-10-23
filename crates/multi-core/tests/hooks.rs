mod common;

use common::*;
use multi_core::doctor::{run_doctor_checks, DoctorOptions};
use multi_core::hooks::{hooks_status, install_hooks, uninstall_hooks, HookState};
use multi_core::sync::{sync, SyncOptions};
use multi_core::Error;
use serde_json::json;

const NO_REMOTES: DoctorOptions = DoctorOptions {
    check_remotes: false,
};

fn hook_script(root: &std::path::Path, name: &str) {
    let path = root.join(".githooks").join(name);
    write(
        &path,
        "#!/bin/sh\necho hook-ran >> \"$(git rev-parse --show-toplevel)/.hook-ran\"\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn hooked_workspace() -> Workspace {
    let ws = workspace();
    ws.update_multi_json(|config| {
        config.insert("hooks".into(), json!({"path": ".githooks"}));
    });
    hook_script(&ws.root, "pre-commit");
    ws
}

#[test]
fn sync_installs_relative_root_and_absolute_subrepo_hooks() {
    let ws = hooked_workspace();
    sync(&ws.root, &SyncOptions::default()).unwrap();
    assert_eq!(
        git(&ws.root, &["config", "--get", "core.hooksPath"]),
        ".githooks"
    );
    let expected_abs = std::fs::canonicalize(ws.root.join(".githooks")).unwrap();
    for repo in &ws.repos {
        assert_eq!(
            git(repo, &["config", "--get", "core.hooksPath"]),
            expected_abs.display().to_string()
        );
    }
    let status = hooks_status(&ws.paths()).unwrap();
    assert!(status.ok);
    assert_eq!(status.hooks, vec!["pre-commit".to_string()]);
    assert!(status.repos.iter().all(|r| r.state == HookState::Installed));
}

#[test]
fn installed_hook_runs_in_root_and_subrepos() {
    let ws = hooked_workspace();
    install_hooks(&ws.paths()).unwrap();
    for repo in std::iter::once(&ws.root).chain(ws.repos.iter()) {
        write(&repo.join("change.txt"), "x");
        commit_all(repo, "trigger hook");
        assert!(
            repo.join(".hook-ran").exists(),
            "hook did not run in {}",
            repo.display()
        );
    }
}

#[test]
fn install_repairs_missing_and_mismatched_values_and_uninstall_only_removes_workspace_values() {
    let ws = hooked_workspace();
    git(ws.repo(0), &["config", "core.hooksPath", "/elsewhere"]);
    let status = hooks_status(&ws.paths()).unwrap();
    assert!(!status.ok);
    assert_eq!(status.repos[1].state, HookState::Mismatch);
    assert_eq!(status.repos[2].state, HookState::Missing);

    let changed = install_hooks(&ws.paths()).unwrap();
    assert_eq!(changed, vec!["(root)", "repo0", "repo1"]);
    assert!(hooks_status(&ws.paths()).unwrap().ok);
    assert!(
        install_hooks(&ws.paths()).unwrap().is_empty(),
        "second install is a no-op"
    );

    git(ws.repo(1), &["config", "core.hooksPath", "/custom"]);
    let removed = uninstall_hooks(&ws.paths()).unwrap();
    assert_eq!(removed, vec!["(root)", "repo0"]);
    assert_eq!(
        git(ws.repo(1), &["config", "--get", "core.hooksPath"]),
        "/custom"
    );
    assert!(!git_ok(&ws.root, &["config", "--get", "core.hooksPath"]));
}

#[test]
fn status_skips_uncloned_and_symlinked_repos() {
    let ws = hooked_workspace();
    let moved = ws.tmp.path().join("moved-repo1");
    std::fs::rename(ws.repo(1), &moved).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&moved, ws.repo(1)).unwrap();
    ws.update_multi_json(|config| {
        config["repos"]
            .as_array_mut()
            .unwrap()
            .push(json!({"url": "https://github.com/test/uncloned"}));
    });
    install_hooks(&ws.paths()).unwrap();
    let status = hooks_status(&ws.paths()).unwrap();
    assert!(status.ok);
    assert_eq!(status.repos[2].state, HookState::Symlinked);
    assert_eq!(status.repos[3].state, HookState::NotCloned);
    assert!(!git_ok(&moved, &["config", "--get", "core.hooksPath"]));
}

#[test]
fn status_flags_missing_dir_and_non_executable_hooks() {
    let ws = workspace();
    ws.update_multi_json(|config| {
        config.insert("hooks".into(), json!({"path": ".githooks"}));
    });
    install_hooks(&ws.paths()).unwrap();
    let status = hooks_status(&ws.paths()).unwrap();
    assert!(!status.hooks_dir_exists);
    assert!(!status.ok);

    write(&ws.root.join(".githooks/pre-push"), "#!/bin/sh\n");
    write(&ws.root.join(".githooks/README.md"), "not a hook");
    let status = hooks_status(&ws.paths()).unwrap();
    assert_eq!(status.hooks, vec!["pre-push".to_string()]);
    #[cfg(unix)]
    {
        assert_eq!(status.non_executable, vec!["pre-push".to_string()]);
        assert!(!status.ok);
    }

    let report = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(report.warnings.iter().any(|w| w.contains("not executable")));
}

#[test]
fn unconfigured_workspace_is_left_alone_and_bad_paths_are_rejected() {
    let ws = workspace();
    assert!(install_hooks(&ws.paths()).unwrap().is_empty());
    assert!(matches!(hooks_status(&ws.paths()), Err(Error::Usage(_))));
    for bad in ["/abs", "../up", ""] {
        ws.update_multi_json(|config| {
            config.insert("hooks".into(), json!({"path": bad}));
        });
        assert!(
            matches!(install_hooks(&ws.paths()), Err(Error::Config(_))),
            "{bad:?}"
        );
        let report = run_doctor_checks(&ws.root, &NO_REMOTES);
        assert!(!report.errors.is_empty());
    }
}

#[test]
fn doctor_warns_when_hooks_not_installed() {
    let ws = hooked_workspace();
    let report = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("Workspace hooks are not installed in: ., repo0, repo1")),
        "{report:?}"
    );
    install_hooks(&ws.paths()).unwrap();
    let report = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(!report
        .warnings
        .iter()
        .any(|w| w.contains("hooks are not installed")));
}
