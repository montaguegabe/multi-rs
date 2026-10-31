mod common;

use common::*;
use multi_core::sync::{sync, SyncOptions};
use multi_core::Paths;
use serde_json::json;

fn sync_no_branch(root: &std::path::Path, install_set: Option<&str>) {
    sync(
        root,
        &SyncOptions {
            ensure_on_same_branch: false,
            install_set: install_set.map(str::to_string),
        },
    )
    .unwrap();
}

#[test]
fn sync_clones_missing_repos_from_remotes_and_mirrors_branch() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let remote_a = create_remote(tmp.path(), "repo-a", &["feature"]);
    let remote_b = create_remote(tmp.path(), "repo-b", &[]);
    let root = tmp.path().join("workspace");
    write(
        &root.join("multi.json"),
        &json!({"repos": [
            {"url": remote_a.to_str().unwrap(), "name": "repo-a"},
            {"url": remote_b.to_str().unwrap(), "name": "repo-b", "fixedBranch": "main"},
        ]})
        .to_string(),
    );
    init_repo(&root);
    commit_all(&root, "init");
    git(&root, &["checkout", "-q", "-b", "feature"]);

    sync(&root, &SyncOptions::default()).unwrap();

    assert_eq!(current_branch(&root.join("repo-a")), "feature");
    assert_eq!(current_branch(&root.join("repo-b")), "main");
    assert!(root.join("README.md").exists(), "README created by sync");
    assert!(root.join(".vscode/settings.json").exists());
    let gitignore = read(&root.join(".gitignore"));
    assert!(gitignore.lines().any(|l| l == "/repo-a/"));
    assert!(gitignore.lines().any(|l| l == "/repo-b"));
    assert!(gitignore.contains(".vscode/launch.json"));
    assert!(read(&root.join(".ignore"))
        .lines()
        .any(|l| l == "!/repo-a/"));

    // Branch missing on the remote falls back to the default branch.
    git(&root, &["checkout", "-q", "-b", "nowhere"]);
    std::fs::remove_dir_all(root.join("repo-a")).unwrap();
    sync(&root, &SyncOptions::default()).unwrap();
    assert_eq!(current_branch(&root.join("repo-a")), "main");
}

#[test]
fn sync_initializes_root_git_repo_and_readme() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    write(
        &root.join("multi.json"),
        &json!({"repos": [{"url": "https://github.com/test/one"}]}).to_string(),
    );
    std::fs::create_dir_all(root.join("one")).unwrap();
    write(&root.join("one/file.txt"), "x");
    sync_no_branch(&root, None);
    assert!(root.join(".git").is_dir());
    let readme = read(&root.join("README.md"));
    assert!(readme.starts_with("# ws\n"));
    assert!(readme.contains("- [one](https://github.com/test/one)"));
}

#[test]
fn install_sets_filter_repos() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    write(
        &workspace.join("multi.json"),
        &json!({"repos": [
            {"url": "https://github.com/test/public", "installSets": ["default", "dev"]},
            {"url": "https://github.com/test/private", "installSets": ["dev"]},
            {"url": "https://github.com/test/legacy"},
        ]})
        .to_string(),
    );
    for name in ["public", "private", "legacy"] {
        write(
            &workspace.join(name).join(".vscode/settings.json"),
            &json!({format!("multi.{name}"): name}).to_string(),
        );
    }

    sync_no_branch(&workspace, None);
    let settings = read_json(&workspace.join(".vscode/settings.json"));
    assert_eq!(settings["multi.public"], "public");
    assert_eq!(settings["multi.private"], "private");
    assert_eq!(settings["multi.legacy"], "legacy");
    assert!(read(&workspace.join(".gitignore")).contains("private/"));

    sync_no_branch(&workspace, Some("default"));
    let settings = read_json(&workspace.join(".vscode/settings.json"));
    assert_eq!(settings["multi.public"], "public");
    assert_eq!(settings["multi.legacy"], "legacy");
    assert!(!settings.contains_key("multi.private"));
    let gitignore = read(&workspace.join(".gitignore"));
    assert!(gitignore.contains("public/"));
    assert!(!gitignore.contains("private/"));

    // Repos without installSets belong to every set, so an unknown set still
    // yields them; a workspace where every repo names sets yields nothing.
    let only_legacy =
        multi_core::load_repos(&Paths::discover(&workspace, Some("nope")).unwrap()).unwrap();
    assert_eq!(
        only_legacy
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        vec!["legacy"]
    );
    write(
        &workspace.join("multi.json"),
        &json!({"repos": [{"url": "https://github.com/test/public", "installSets": ["default"]}]})
            .to_string(),
    );
    assert!(matches!(
        multi_core::load_repos(&Paths::discover(&workspace, Some("nope")).unwrap()),
        Err(multi_core::Error::NoRepositories(_))
    ));
}

#[test]
fn agents_generation_is_opt_in() {
    let ws = workspace();
    write(&ws.root.join("AGENTS.parts/10-a.md"), "Root part A\n");
    write(&ws.root.join("AGENTS.parts/20-b.md"), "\nRoot part B\n\n");
    write(&ws.repo(0).join("AGENTS.parts/x.md"), "Repo0 guidance\n");
    write(&ws.root.join("AGENTS.md"), "manual\n");

    sync_no_branch(&ws.root, None);
    assert_eq!(
        read(&ws.root.join("AGENTS.md")),
        "manual\n",
        "disabled leaves manual files alone"
    );
    assert!(!ws.repo(0).join("AGENTS.md").exists());

    ws.update_multi_json(|config| {
        config.insert("agentInstructions".into(), json!({"enabled": true}));
        config["repos"][0]["description"] = json!("Backend API");
    });
    sync_no_branch(&ws.root, None);
    let root_agents = read(&ws.root.join("AGENTS.md"));
    assert_eq!(
        root_agents,
        "This workspace contains multiple repositories:\n\n- `repo0`: Backend API\n\nRoot part A\n\nRoot part B\n"
    );
    assert_eq!(read(&ws.root.join("CLAUDE.md")), root_agents);
    assert_eq!(read(&ws.repo(0).join("AGENTS.md")), "Repo0 guidance\n");
    assert!(!ws.repo(1).join("AGENTS.md").exists());
    assert!(read(&ws.root.join(".gitignore")).contains("CLAUDE.md"));
    assert!(read(&ws.repo(0).join(".gitignore")).contains("AGENTS.md"));
    assert!(
        !ws.repo(1).join(".gitignore").exists()
            || !read(&ws.repo(1).join(".gitignore")).contains("AGENTS.md")
    );

    ws.update_multi_json(|config| {
        config["agentInstructions"]["includeRepoDescriptions"] = json!(false);
        config["agentInstructions"]["partsDir"] = json!("docs/parts");
    });
    write(&ws.root.join("docs/parts/only.md"), "Only\n");
    sync_no_branch(&ws.root, None);
    assert_eq!(read(&ws.root.join("AGENTS.md")), "Only\n");
}

#[test]
fn monorepo_mode_syncs_github_workflows_and_skips_cloning() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("mono");
    write(
        &root.join("multi.json"),
        &json!({"monoRepo": true, "repos": [{"name": "packages/web"}, {"name": "packages/api"}]})
            .to_string(),
    );
    init_repo(&root);
    write(
        &root.join("packages/web/.github/workflows/ci.yml"),
        "name: CI\non:\n  push:\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npm test\n",
    );
    write(
        &root.join("packages/api/.github/workflows/ci.yml"),
        "name: API\njobs:\n  test:\n    runs-on: ubuntu-latest\n    defaults:\n      run:\n        working-directory: custom\n    steps: []\n",
    );
    write(&root.join(".github/workflows/manual.yml"), "name: manual\n");
    write(
        &root.join(".github/workflows/stale.yml"),
        "# multi-generated: github-workflow\nname: stale\n",
    );

    sync_no_branch(&root, None);

    let workflows = root.join(".github/workflows");
    let web = read(&workflows.join("ci.yml"));
    assert!(web.starts_with("# multi-generated: github-workflow\n# source: packages/web/.github/workflows/ci.yml\n# repo: packages/web\n\n"));
    assert!(web.contains("working-directory: packages/web"));
    let api = read(&workflows.join("packages-api--ci.yml"));
    assert!(api.contains("working-directory: custom"));
    assert!(!api.contains("packages/api\n") || api.contains("# repo: packages/api"));
    assert!(workflows.join("manual.yml").exists());
    assert!(!workflows.join("stale.yml").exists());
    // No repo-directories block in monorepo mode.
    assert!(
        !root.join(".gitignore").exists()
            || !read(&root.join(".gitignore")).contains("repo-directories")
    );
    assert!(root.join(".vscode/settings.json").exists());

    // github sync is a no-op outside monorepo mode.
    let ws = workspace();
    write(&ws.repo(0).join(".github/workflows/ci.yml"), "name: x\n");
    sync_no_branch(&ws.root, None);
    assert!(!ws.root.join(".github/workflows/ci.yml").exists());
}

#[test]
fn devcontainer_is_copied_and_prefixed() {
    let ws = workspace();
    write(
        &ws.repo(1).join(".devcontainer/devcontainer.json"),
        &json!({"dockerComposeFile": "../docker-compose.yml", "workspaceFolder": "${workspaceFolder}/src"}).to_string(),
    );
    sync_no_branch(&ws.root, None);
    let copied = read_json(&ws.root.join(".devcontainer/devcontainer.json"));
    assert_eq!(copied["dockerComposeFile"], "../repo1/docker-compose.yml");
    assert_eq!(copied["workspaceFolder"], "${workspaceFolder}/repo1/src");
}

#[test]
fn symlinks_reuse_registered_clones_when_enabled() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let remote = create_remote(tmp.path(), "shared", &[]);
    let existing = tmp.path().join("elsewhere/shared");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            existing.to_str().unwrap(),
        ],
    );
    multi_core::registry::register_repo(remote.to_str().unwrap(), &existing);

    let root = tmp.path().join("ws");
    write(
        &root.join("multi.json"),
        &json!({"allowSymlinks": true, "repos": [{"url": remote.to_str().unwrap(), "name": "shared", "allowSymlink": true}]}).to_string(),
    );
    sync_no_branch(&root, None);
    let link = root.join("shared");
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    // The registry stores canonical paths, so compare against the canonical
    // clone location (on macOS the temp dir lives under `/private/var`).
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        std::fs::canonicalize(&existing).unwrap()
    );
}
