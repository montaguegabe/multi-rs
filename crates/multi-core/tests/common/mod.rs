//! Shared fixtures for integration tests: temporary git repos, bare remotes
//! and multi workspaces.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::{json, Value};
use tempfile::TempDir;

/// Redirect `~/.multi` state to a scratch directory for the whole test
/// process so tests never touch the real registry.
pub fn setup() {
    static HOME: OnceLock<TempDir> = OnceLock::new();
    let home = HOME.get_or_init(|| tempfile::tempdir().expect("tempdir"));
    multi_core::fsx::set_multi_home_dir(home.path().to_path_buf());
}

pub fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {} failed in {}: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn current_branch(repo: &Path) -> String {
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
}

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub fn read_json(path: &Path) -> serde_json::Map<String, Value> {
    multi_core::jsonc::soft_read_json_file(path)
}

/// `git init -b main` with a local identity configured.
pub fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.name", "Test User"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "commit.gpgsign", "false"]);
}

pub fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "--allow-empty", "-m", message]);
}

/// Commit with an explicit author/committer timestamp (unix seconds), so
/// history ordering in tests is deterministic.
pub fn commit_all_at(repo: &Path, message: &str, unix_seconds: i64) {
    git(repo, &["add", "-A"]);
    let date = format!("{unix_seconds} +0000");
    let output = Command::new("git")
        .args(["commit", "-q", "--allow-empty", "-m", message])
        .current_dir(repo)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repo with one commit on `main`.
pub fn seed_repo(path: &Path, readme: &str) {
    init_repo(path);
    write(&path.join("README.md"), readme);
    commit_all(path, "Initial commit");
}

/// Create a bare remote populated from a seed repo. Returns the remote path
/// (usable as a clone URL). Extra branches are pushed too.
pub fn create_remote(base: &Path, name: &str, branches: &[&str]) -> PathBuf {
    let seed = base.join("seeds").join(name);
    let remote = base.join("remotes").join(name);
    seed_repo(&seed, &format!("# {name}\n"));
    write(&seed.join(".gitignore"), ".secret\n");
    commit_all(&seed, "Add gitignore");
    for branch in branches {
        git(&seed, &["checkout", "-q", "-b", branch]);
        write(&seed.join(format!("{branch}.txt")), branch);
        commit_all(&seed, &format!("Add {branch}"));
        git(&seed, &["checkout", "-q", "main"]);
    }
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
    git(
        base,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-q", "origin", "main:main"]);
    for branch in branches {
        git(
            &seed,
            &["push", "-q", "origin", &format!("{branch}:{branch}")],
        );
    }
    remote
}

/// A workspace fixture: root repo plus `repo0`/`repo1` sub-repos, each with
/// a bare remote and upstream tracking, already synced once.
pub struct Workspace {
    pub tmp: TempDir,
    pub root: PathBuf,
    pub repos: Vec<PathBuf>,
}

impl Workspace {
    pub fn repo(&self, index: usize) -> &Path {
        &self.repos[index]
    }

    pub fn multi_json(&self) -> serde_json::Map<String, Value> {
        serde_json::from_str(&read(&self.root.join("multi.json"))).unwrap()
    }

    pub fn write_multi_json(&self, value: &Value) {
        write(
            &self.root.join("multi.json"),
            &serde_json::to_string_pretty(value).unwrap(),
        );
    }

    pub fn update_multi_json(&self, update: impl FnOnce(&mut serde_json::Map<String, Value>)) {
        let mut config = self.multi_json();
        update(&mut config);
        self.write_multi_json(&Value::Object(config));
    }

    pub fn paths(&self) -> multi_core::Paths {
        multi_core::Paths::discover(&self.root, None).unwrap()
    }
}

/// Build the standard fixture: a root repo at `<tmp>/root` whose multi.json
/// lists `repo0` and `repo1` with placeholder URLs; sub-repos are seeded
/// in place (no network) and given bare remotes with upstream tracking.
pub fn workspace() -> Workspace {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    let names = ["repo0", "repo1"];

    write(
        &root.join("multi.json"),
        &serde_json::to_string_pretty(&json!({
            "repos": names.iter().map(|n| json!({"url": format!("https://github.com/test/{n}")})).collect::<Vec<_>>()
        }))
        .unwrap(),
    );
    init_repo(&root);
    write(&root.join("README.md"), "# Root Repository");
    commit_all(&root, "Initial commit");

    let mut repos = Vec::new();
    for name in names {
        let repo = root.join(name);
        seed_repo(&repo, &format!("# Sub Repository {name}"));
        repos.push(repo);
    }

    multi_core::sync::sync(&root, &multi_core::sync::SyncOptions::default()).unwrap();
    commit_all(&root, "Post-sync commit");
    for repo in &repos {
        if !git(repo, &["status", "--porcelain"]).is_empty() {
            commit_all(repo, "Post-sync commit");
        }
    }

    let remotes = tmp.path().join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();
    for (name, repo) in
        std::iter::once(("root", &root)).chain(names.iter().copied().zip(repos.iter()))
    {
        let remote = remotes.join(format!("{name}.git"));
        git(
            tmp.path(),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                remote.to_str().unwrap(),
            ],
        );
        git(repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(repo, &["push", "-q", "-u", "origin", "main:main"]);
    }

    Workspace { tmp, root, repos }
}

/// A workspace whose sub-repos are cloned from real (file) remotes, so
/// `multi sync` and `multi worktree add` exercise cloning.
pub fn workspace_with_remotes(repo_configs: &[Value], extra: Value) -> Workspace {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("workspace");
    std::fs::create_dir_all(&root).unwrap();

    let mut config = json!({"repos": repo_configs});
    if let (Value::Object(target), Value::Object(extra)) = (&mut config, extra) {
        for (k, v) in extra {
            target.insert(k, v);
        }
    }
    write(
        &root.join("multi.json"),
        &serde_json::to_string_pretty(&config).unwrap(),
    );

    let mut gitignore: Vec<String> = vec![".env".into(), ".cache/".into()];
    for repo in repo_configs {
        gitignore.push(format!("{}/", repo["name"].as_str().unwrap()));
    }
    write(
        &root.join(".gitignore"),
        &format!("{}\n", gitignore.join("\n")),
    );
    write(&root.join("README.md"), "# Workspace\n");
    init_repo(&root);
    commit_all(&root, "Initial commit");

    let mut repos = Vec::new();
    for repo in repo_configs {
        let name = repo["name"].as_str().unwrap();
        let url = repo["url"].as_str().unwrap();
        let destination = root.join(name);
        git(&root, &["clone", "-q", url, destination.to_str().unwrap()]);
        git(&destination, &["config", "user.name", "Test User"]);
        git(&destination, &["config", "user.email", "test@example.com"]);
        repos.push(destination);
    }
    Workspace { tmp, root, repos }
}
