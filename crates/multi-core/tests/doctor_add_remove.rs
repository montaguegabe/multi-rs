mod common;

use common::*;
use multi_core::add_remove::{add_repo, remove_repo, RemoveOptions};
use multi_core::convert_monorepo::convert_to_monorepo;
use multi_core::doctor::{run_doctor_checks, run_doctor_fixes, DoctorOptions};
use multi_core::init::{run_init, InitInputs};
use serde_json::json;

const NO_REMOTES: DoctorOptions = DoctorOptions {
    check_remotes: false,
};

#[test]
fn doctor_reports_missing_config_and_mismatches() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let report = run_doctor_checks(tmp.path(), &NO_REMOTES);
    assert!(report.errors[0].contains("Could not find multi.json"));

    write(&tmp.path().join("multi.json"), "not json");
    let report = run_doctor_checks(tmp.path(), &NO_REMOTES);
    assert!(report.errors[0].contains("Could not parse multi.json"));

    let ws = workspace();
    let clean = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(clean.errors.is_empty(), "{clean:?}");
    assert!(clean
        .warnings
        .iter()
        .any(|w| w.contains("missing a non-empty `description`: repo0, repo1")));
    assert!(clean.should_fail(true));
    assert!(!clean.should_fail(false));

    ws.update_multi_json(|config| {
        config["repos"][0]["description"] = json!("One");
        config["repos"][1]["description"] = json!("Two");
    });
    std::fs::remove_dir_all(ws.repo(1)).unwrap();
    seed_repo(&ws.root.join("stray"), "stray");
    let report = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("not declared in multi.json: stray")),
        "{report:?}"
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("not found on disk: repo1")),
        "{report:?}"
    );
    assert!(!report.warnings.iter().any(|w| w.contains("description")));
}

#[test]
fn doctor_detects_and_fixes_tracked_subrepos() {
    let ws = workspace();
    // Force-track a file inside repo0 in the root index. Git would record a
    // nested repo as a gitlink, so hide repo0's .git while adding.
    write(&ws.repo(0).join("tracked.txt"), "x");
    std::fs::rename(ws.repo(0).join(".git"), ws.repo(0).join(".git-hidden")).unwrap();
    git(&ws.root, &["add", "-f", "repo0/tracked.txt"]);
    commit_all(&ws.root, "track subrepo file");
    std::fs::rename(ws.repo(0).join(".git-hidden"), ws.repo(0).join(".git")).unwrap();
    let report = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("tracked in the workspace git index")),
        "{report:?}"
    );

    assert_eq!(
        run_doctor_fixes(&ws.root).unwrap(),
        vec!["repo0".to_string()]
    );
    assert!(
        ws.repo(0).join("tracked.txt").exists(),
        "files remain on disk"
    );
    let report = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(!report
        .warnings
        .iter()
        .any(|w| w.contains("tracked in the workspace git index")));
    assert!(run_doctor_fixes(&ws.root).unwrap().is_empty());
}

#[test]
fn doctor_monorepo_warns_on_nested_git_and_missing_url_in_standard_mode() {
    let ws = workspace();
    ws.update_multi_json(|config| {
        config.insert("monoRepo".into(), json!(true));
    });
    let report = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("Found nested git repos: repo0, repo1")),
        "{report:?}"
    );

    ws.update_multi_json(|config| {
        config.insert("monoRepo".into(), json!(false));
        config["repos"][0] = json!({"name": "repo0"});
    });
    let report = run_doctor_checks(&ws.root, &NO_REMOTES);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("must contain 'url' field")),
        "{report:?}"
    );
}

#[test]
fn doctor_remote_check_reports_unreachable_remotes() {
    let ws = workspace();
    let report = run_doctor_checks(
        &ws.root,
        &DoctorOptions {
            check_remotes: true,
        },
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("unreachable remotes")),
        "{report:?}"
    );
}

#[test]
fn add_and_remove_repo_round_trip() {
    setup();
    let scratch = tempfile::tempdir().unwrap();
    let remote = create_remote(scratch.path(), "t-ide-cli", &[]);
    let ws = workspace();
    // Rename root to exercise prefix shortening (`t-ide` workspace).
    let root = ws.tmp.path().join("t-ide");
    std::fs::rename(&ws.root, &root).unwrap();

    let name = add_repo(&root, remote.to_str().unwrap(), None).unwrap();
    assert_eq!(name, "cli");
    let config =
        serde_json::from_str::<serde_json::Value>(&read(&root.join("multi.json"))).unwrap();
    assert_eq!(config["repos"][2]["name"], "cli");
    assert!(root.join("cli/.git").exists());
    assert!(read(&root.join(".gitignore")).lines().any(|l| l == "/cli/"));

    let err = add_repo(&root, remote.to_str().unwrap(), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("already exists in multi.json"), "{err}");
    let err = add_repo(&root, "https://github.com/test/other", Some("repo0"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("name 'repo0' already exists"), "{err}");

    write(&root.join("cli/dirty.txt"), "x");
    assert!(remove_repo(
        &root,
        "cli",
        &RemoveOptions {
            delete: true,
            force: false
        }
    )
    .is_err());
    remove_repo(
        &root,
        "cli",
        &RemoveOptions {
            delete: true,
            force: true,
        },
    )
    .unwrap();
    assert!(!root.join("cli").exists());
    let config =
        serde_json::from_str::<serde_json::Value>(&read(&root.join("multi.json"))).unwrap();
    assert_eq!(config["repos"].as_array().unwrap().len(), 2);
    assert!(!read(&root.join(".gitignore")).lines().any(|l| l == "/cli/"));
    assert!(!read(&root.join(".ignore")).lines().any(|l| l == "!/cli/"));
    assert!(remove_repo(&root, "cli", &RemoveOptions::default()).is_err());
}

#[test]
fn init_non_interactive_writes_config_syncs_and_commits() {
    setup();
    let scratch = tempfile::tempdir().unwrap();
    let remote = create_remote(scratch.path(), "svc-api", &[]);
    let root = scratch.path().join("svc");
    init_repo(&root);
    let inputs = InitInputs {
        repo_urls: vec![remote.to_str().unwrap().to_string()],
        repo_descriptions: vec!["Backend".to_string()],
        github_visibility: "private".into(),
        github_clone_protocol: "https".into(),
        ..Default::default()
    };
    run_init(&root, &inputs).unwrap();
    let config =
        serde_json::from_str::<serde_json::Value>(&read(&root.join("multi.json"))).unwrap();
    assert_eq!(config["repos"][0]["name"], "api");
    assert_eq!(config["repos"][0]["description"], "Backend");
    assert!(root.join("api/.git").exists());
    assert!(root.join("README.md").exists());
    assert_eq!(
        git(&root, &["log", "-1", "--format=%s"]),
        "Multi init: Configure multi workspace"
    );
    assert!(git(&root, &["status", "--porcelain"]).is_empty());
}

#[test]
fn convert_monorepo_removes_nested_git_and_sets_flag() {
    let ws = workspace();
    convert_to_monorepo(&ws.root).unwrap();
    assert!(!ws.repo(0).join(".git").exists());
    assert!(!ws.repo(1).join(".git").exists());
    assert_eq!(ws.multi_json()["monoRepo"], true);
    assert!(!read(&ws.root.join(".gitignore"))
        .lines()
        .any(|l| l == "/repo0/"));
    assert!(matches!(
        convert_to_monorepo(&ws.root),
        Err(multi_core::Error::Usage(_))
    ));
}
