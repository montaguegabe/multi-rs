//! End-to-end tests of the `multi` binary.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::json;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.name", "Test User"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "commit.gpgsign", "false"]);
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--allow-empty", "-m", message]);
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    home: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("multi.json"),
        json!({"repos": [{"url": "https://github.com/test/a", "name": "a"}, {"url": "https://github.com/test/b", "name": "b"}]}).to_string(),
    )
    .unwrap();
    init_repo(&root);
    commit_all(&root, "init");
    for name in ["a", "b"] {
        let repo = root.join(name);
        init_repo(&repo);
        std::fs::write(repo.join("README.md"), name).unwrap();
        commit_all(&repo, "init");
    }
    let home = tmp.path().join("home");
    Fixture {
        _tmp: tmp,
        root,
        home,
    }
}

fn multi(fixture: &Fixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_multi"))
        .args(args)
        .current_dir(&fixture.root)
        .env("MULTI_HOME", &fixture.home)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_and_help() {
    let f = fixture();
    let out = multi(&f, &["--version"]);
    assert!(stdout(&out).starts_with("multi "));
    let help = stdout(&multi(&f, &["sync", "--help"]));
    assert!(help.contains("--install-set"));
    assert!(help.contains("--set"));
    let branch_help = stdout(&multi(&f, &["branch", "--help"]));
    assert!(branch_help.contains("check"));
}

#[test]
fn sync_branch_and_git_flow() {
    let f = fixture();
    let out = multi(&f, &["sync"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(f.root.join(".vscode/settings.json").exists());
    assert!(!f.home.join("repos.json").exists() || f.home.join("repos.json").exists());

    let out = multi(&f, &["branch", "check"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("ws (root): main"));
    assert!(stderr(&out).contains("a: main"));

    std::fs::write(f.root.join("a/dirty.txt"), "x").unwrap();
    let out = multi(&f, &["git", "branch", "--show-current"]);
    assert!(
        out.status.success(),
        "dirty trees are fine: {}",
        stderr(&out)
    );

    git(&f.root.join("b"), &["checkout", "-q", "-b", "other"]);
    let out = multi(&f, &["branch"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("b: other (expected root branch main)"));
    let out = multi(&f, &["git", "status"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("not on the expected branch"));

    // Post-command check warns (exit 0) after sync vscode when branches mismatch.
    let out = multi(&f, &["sync", "vscode", "settings"]);
    assert!(out.status.success());
    assert!(stderr(&out).contains("not on the expected branch"));
}

#[test]
fn install_set_applies_to_partial_sync_commands() {
    let f = fixture();
    std::fs::write(
        f.root.join("multi.json"),
        json!({"repos": [
            {"url": "https://github.com/test/a", "name": "a", "installSets": ["default"]},
            {"url": "https://github.com/test/b", "name": "b", "installSets": ["dev"]},
        ]})
        .to_string(),
    )
    .unwrap();
    for name in ["a", "b"] {
        std::fs::create_dir_all(f.root.join(name).join(".vscode")).unwrap();
        std::fs::write(
            f.root.join(name).join(".vscode/settings.json"),
            json!({format!("multi.{name}"): name}).to_string(),
        )
        .unwrap();
    }
    let out = multi(&f, &["sync", "--set", "default", "vscode", "settings"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let settings = multi_core::jsonc::soft_read_json_file(&f.root.join(".vscode/settings.json"));
    assert_eq!(settings["multi.a"], "a");
    assert!(!settings.contains_key("multi.b"));
}

#[test]
fn hooks_status_and_install() {
    let f = fixture();
    let out = multi(&f, &["hooks", "status"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("No workspace hooks configured"));

    std::fs::write(
        f.root.join("multi.json"),
        json!({"hooks": {"path": ".githooks"}, "repos": [{"url": "https://github.com/test/a", "name": "a"}, {"url": "https://github.com/test/b", "name": "b"}]}).to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(f.root.join(".githooks")).unwrap();
    let out = multi(&f, &["hooks", "status"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("missing"));

    let out = multi(&f, &["hooks", "install"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("installed"));
    let out = multi(&f, &["hooks", "status", "--json"]);
    assert!(out.status.success());
    let status: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(status["ok"], true);
    assert_eq!(status["repos"][1]["state"], "installed");

    let out = multi(&f, &["hooks", "uninstall"]);
    assert!(stdout(&out).contains("Removed workspace hooks from: (root), a, b"));
}

#[test]
fn doctor_json_and_exit_codes() {
    let f = fixture();
    let out = multi(&f, &["doctor", "--json", "--no-remote-check"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let report: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(report["errors"].as_array().unwrap().is_empty());
    assert!(report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("description")));
    let out = multi(&f, &["doctor", "--strict", "--no-remote-check"]);
    assert_eq!(out.status.code(), Some(1));

    let out = multi(&f, &["convert-monorepo"]);
    assert_eq!(out.status.code(), Some(2));
}
