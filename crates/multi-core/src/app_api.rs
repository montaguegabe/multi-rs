//! Read-only project inspection used by the desktop app and the local JSON
//! service: status, branches, combined history and diffs for a project and
//! its sub-repos.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::doctor::{run_doctor_checks, DoctorOptions};
use crate::fsx;
use crate::git::{git_output, git_output_timeout, git_top_level, is_git_repo_root, stdout_string};
use crate::paths::Paths;
use crate::repos::{load_repos, Repository};

const HISTORY_GROUP_WINDOW_MS: i64 = 60_000;
const HISTORY_MAX_COMMITS_PER_REPO: usize = 250;
const GIT_HISTORY_FORMAT: &str = "%H%x1f%h%x1f%an%x1f%ae%x1f%at%x1f%s%x1f%b%x1e";
const METADATA_CACHE_TTL: Duration = Duration::from_secs(10);
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(12);
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepoStatus {
    #[serde(rename = "clean")]
    Clean,
    #[serde(rename = "dirty")]
    Dirty,
    #[serde(rename = "out-of-sync")]
    OutOfSync,
    #[serde(rename = "no_git")]
    NoGit,
    #[serde(rename = "missing")]
    Missing,
    #[serde(rename = "unknown")]
    Unknown,
}

impl RepoStatus {
    fn severity(self) -> u8 {
        match self {
            RepoStatus::Clean => 0,
            RepoStatus::Unknown => 1,
            RepoStatus::NoGit => 2,
            RepoStatus::OutOfSync => 3,
            RepoStatus::Dirty => 4,
            RepoStatus::Missing => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSyncState {
    pub ahead: i64,
    pub behind: i64,
    pub has_upstream: bool,
    pub is_repo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubRepoPayload {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoBranchPayload {
    pub name: String,
    pub path: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorResultPayload {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub path: String,
    pub name: String,
    pub last_modified_ms: Option<i64>,
    pub last_commit_ms: Option<i64>,
    pub status: RepoStatus,
    pub doctor_result: Option<DoctorResultPayload>,
    pub sub_repos: Option<Vec<SubRepoPayload>>,
    pub branches: Vec<RepoBranchPayload>,
    /// Linked git worktrees of this trunk workspace, populated by the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees: Option<Vec<ProjectSummary>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryDiff {
    pub path: String,
    pub name: String,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCommit {
    pub hash: String,
    pub short_hash: String,
    pub subject: String,
    pub description: String,
    pub author_name: String,
    pub author_email: String,
    pub authored_at_ms: i64,
    pub authored_at_iso: String,
    pub repo_path: String,
    pub repo_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryGroup {
    pub id: String,
    pub newest_authored_at_ms: i64,
    pub oldest_authored_at_ms: i64,
    pub authored_at_iso: String,
    pub repo_names: Vec<String>,
    pub commits: Vec<HistoryCommit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCommitRef {
    pub repo_path: String,
    pub repo_name: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRepoDiff {
    pub repo_path: String,
    pub repo_name: String,
    pub diff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetail {
    pub repo_path: String,
    pub repositories: Vec<RepositoryDiff>,
    pub history_groups: Vec<HistoryGroup>,
    pub sync_states_by_repo_path: BTreeMap<String, RepoSyncState>,
    pub sub_repos: Option<Vec<SubRepoPayload>>,
    pub branches: Vec<RepoBranchPayload>,
}

fn git_ok(cwd: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    match git_output_timeout(cwd, args, timeout) {
        Ok(Some(output)) if output.status.success() => Some(stdout_string(&output)),
        _ => None,
    }
}

/// Sub-repos declared by a `multi.json` directly inside `repo_path` (never a
/// parent directory), sorted by name. `None` when it is not a workspace.
fn load_subrepos(repo_path: &Path) -> Option<Vec<Repository>> {
    if !repo_path.join("multi.json").exists() {
        return None;
    }
    let paths = Paths::at_root(repo_path, None);
    let mut repos = load_repos(&paths).ok()?;
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    Some(repos)
}

fn subrepo_payloads(repo_path: &Path) -> Option<Vec<SubRepoPayload>> {
    let repos = load_subrepos(repo_path)?;
    if repos.is_empty() {
        return None;
    }
    Some(
        repos
            .into_iter()
            .map(|repo| SubRepoPayload {
                name: repo.name,
                path: repo.path.display().to_string(),
            })
            .collect(),
    )
}

/// Public multi.json sub-repo listing (empty when not a workspace).
pub fn get_project_subrepos(repo_path: &Path) -> Vec<SubRepoPayload> {
    subrepo_payloads(repo_path).unwrap_or_default()
}

/// Names of repos declared in `repo_path`'s own multi.json.
pub fn list_project_repo_names(repo_path: &Path) -> Vec<String> {
    load_subrepos(repo_path)
        .unwrap_or_default()
        .into_iter()
        .map(|repo| repo.name)
        .collect()
}

fn project_name(project_path: &Path) -> String {
    project_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| project_path.display().to_string())
}

fn describe_git_head(repo_path: &Path) -> Option<String> {
    if !repo_path.exists() || !repo_path.join(".git").exists() || !is_git_repo_root(repo_path) {
        return None;
    }
    if let Some(branch) = git_ok(
        repo_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        GIT_TIMEOUT,
    ) {
        let branch = branch.trim().to_string();
        if !branch.is_empty() {
            return Some(branch);
        }
    }
    let short = git_ok(repo_path, &["rev-parse", "--short", "HEAD"], GIT_TIMEOUT)?;
    let short = short.trim();
    (!short.is_empty()).then(|| format!("(detached at {short})"))
}

struct RepoEntry {
    name: String,
    path: PathBuf,
}

fn unique_repo_entries(project_path: &Path) -> Vec<RepoEntry> {
    let mut entries = vec![RepoEntry {
        name: project_name(project_path),
        path: project_path.to_path_buf(),
    }];
    for repo in load_subrepos(project_path).unwrap_or_default() {
        entries.push(RepoEntry {
            name: repo.name,
            path: repo.path,
        });
    }
    let mut seen: HashSet<PathBuf> = HashSet::new();
    entries
        .into_iter()
        .filter(|entry| seen.insert(fsx::resolve(&entry.path)))
        .collect()
}

pub fn get_project_branches(project_path: &Path) -> Vec<RepoBranchPayload> {
    unique_repo_entries(project_path)
        .into_iter()
        .filter_map(|entry| {
            describe_git_head(&entry.path).map(|branch| RepoBranchPayload {
                name: entry.name,
                path: entry.path.display().to_string(),
                branch,
            })
        })
        .collect()
}

fn git_status_internal(repo_path: &Path, ignore_untracked: bool) -> RepoStatus {
    if !repo_path.exists() {
        return RepoStatus::Missing;
    }
    if !repo_path.join(".git").exists() {
        return RepoStatus::NoGit;
    }
    let mut args = vec!["status", "--porcelain"];
    if ignore_untracked {
        args.push("--untracked-files=no");
    }
    // A directory with a .git entry that git itself rejects (corrupt or
    // locked repo) is neither clean nor dirty; report it as unknown.
    let Some(porcelain) = git_ok(repo_path, &args, GIT_TIMEOUT) else {
        return RepoStatus::Unknown;
    };
    if !porcelain.trim().is_empty() {
        return RepoStatus::Dirty;
    }
    match ahead_behind(repo_path) {
        Some((0, 0)) => RepoStatus::Clean,
        _ => RepoStatus::OutOfSync,
    }
}

fn ahead_behind(repo_path: &Path) -> Option<(i64, i64)> {
    let output = git_ok(
        repo_path,
        &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
        GIT_TIMEOUT,
    )?;
    let mut parts = output.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

pub fn get_repo_status(repo_path: &Path) -> RepoStatus {
    git_status_internal(repo_path, false)
}

pub fn get_repo_sync_state(repo_path: &Path) -> RepoSyncState {
    if !repo_path.exists() || !repo_path.join(".git").exists() {
        return RepoSyncState {
            ahead: 0,
            behind: 0,
            has_upstream: false,
            is_repo: false,
        };
    }
    match ahead_behind(repo_path) {
        Some((ahead, behind)) => RepoSyncState {
            ahead,
            behind,
            has_upstream: true,
            is_repo: true,
        },
        None => RepoSyncState {
            ahead: 0,
            behind: 0,
            has_upstream: false,
            is_repo: true,
        },
    }
}

fn latest_commit_ms(repo_path: &Path) -> Option<i64> {
    if !repo_path.exists() || !repo_path.join(".git").exists() || !is_git_repo_root(repo_path) {
        return None;
    }
    let stdout = git_ok(repo_path, &["log", "-1", "--format=%at"], GIT_TIMEOUT)?;
    stdout.trim().parse::<i64>().ok().map(|s| s * 1000)
}

fn worst_status(a: RepoStatus, b: RepoStatus) -> RepoStatus {
    if a.severity() >= b.severity() {
        a
    } else {
        b
    }
}

/// Worst status across the project and its sub-repos. Missing/no-git
/// sub-repos are ignored; the root ignores untracked files.
pub fn get_project_status(project_path: &Path) -> RepoStatus {
    let Some(subrepos) = load_subrepos(project_path).filter(|s| !s.is_empty()) else {
        return get_repo_status(project_path);
    };
    let mut worst = git_status_internal(project_path, true);
    if matches!(worst, RepoStatus::Missing | RepoStatus::NoGit) {
        worst = RepoStatus::Clean;
    }
    for repo in subrepos {
        let status = git_status_internal(&repo.path, false);
        if matches!(status, RepoStatus::Missing | RepoStatus::NoGit) {
            continue;
        }
        worst = worst_status(worst, status);
    }
    worst
}

pub fn get_project_last_commit_ms(project_path: &Path) -> Option<i64> {
    let mut candidates = vec![project_path.to_path_buf()];
    candidates.extend(
        load_subrepos(project_path)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.path),
    );
    candidates.iter().filter_map(|p| latest_commit_ms(p)).max()
}

/// Working-tree diff of `repo_path` including untracked files rendered as
/// additions.
pub fn get_repo_diff(repo_path: &Path) -> String {
    if !is_git_repo_root(repo_path) {
        return String::new();
    }
    let tracked = match git_output(repo_path, &["diff", "HEAD"]) {
        Ok(output) if output.status.success() => stdout_string(&output),
        _ => {
            // No HEAD yet (initial commit): fall back to unstaged + staged.
            let unstaged = git_output(repo_path, &["diff"])
                .map(|o| stdout_string(&o))
                .unwrap_or_default();
            let staged = git_output(repo_path, &["diff", "--cached"])
                .map(|o| stdout_string(&o))
                .unwrap_or_default();
            unstaged + &staged
        }
    };
    let untracked_list = git_output(repo_path, &["ls-files", "--others", "--exclude-standard"])
        .map(|o| stdout_string(&o))
        .unwrap_or_default();
    let mut untracked_diffs = String::new();
    for file in untracked_list.lines().filter(|l| !l.trim().is_empty()) {
        // `git diff --no-index` exits 1 when files differ, so read stdout
        // regardless of status.
        if let Ok(output) = git_output(repo_path, &["diff", "--no-index", "--", "/dev/null", file])
        {
            untracked_diffs.push_str(&stdout_string(&output));
        }
    }
    tracked + &untracked_diffs
}

fn iso_utc(authored_at_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(authored_at_ms)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.fZ").to_string())
        .map(|s| s.replace(".000Z", "Z"))
        .unwrap_or_default()
}

fn history_commit_from_record(
    record: &str,
    repo_path: &Path,
    repo_name: &str,
) -> Option<HistoryCommit> {
    let mut fields = record.splitn(7, '\x1f');
    let hash = fields.next()?.to_string();
    let short_hash = fields.next()?.to_string();
    let author_name = fields.next().unwrap_or("").to_string();
    let author_email = fields.next().unwrap_or("").to_string();
    let authored_at_unix = fields.next().unwrap_or("");
    let subject = fields.next().unwrap_or("").to_string();
    let body = fields.next().unwrap_or("");
    let description = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string();
    let authored_at_ms = authored_at_unix.trim().parse::<i64>().ok()? * 1000;
    if hash.is_empty() || short_hash.is_empty() || subject.is_empty() {
        return None;
    }
    Some(HistoryCommit {
        hash,
        short_hash,
        subject,
        description,
        author_name,
        author_email,
        authored_at_ms,
        authored_at_iso: iso_utc(authored_at_ms),
        repo_path: repo_path.display().to_string(),
        repo_name: repo_name.to_string(),
    })
}

pub fn get_history_for_repo(repo_path: &Path, repo_name: &str) -> Vec<HistoryCommit> {
    if !repo_path.exists() || !is_git_repo_root(repo_path) {
        return Vec::new();
    }
    let max_count = format!("--max-count={HISTORY_MAX_COMMITS_PER_REPO}");
    let pretty = format!("--pretty=format:{GIT_HISTORY_FORMAT}");
    let stdout = match git_output(repo_path, &["log", "--date-order", &max_count, &pretty]) {
        Ok(output) if output.status.success() => stdout_string(&output),
        _ => return Vec::new(),
    };
    stdout
        .split('\x1e')
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .filter_map(|record| history_commit_from_record(record, repo_path, repo_name))
        .collect()
}

/// Group commits (across repos) that were authored within one minute of each
/// other, newest first.
pub fn group_history_commits(mut commits: Vec<HistoryCommit>) -> Vec<HistoryGroup> {
    if commits.is_empty() {
        return Vec::new();
    }
    commits.sort_by(|a, b| {
        b.authored_at_ms
            .cmp(&a.authored_at_ms)
            .then_with(|| a.hash.cmp(&b.hash))
    });
    let mut groups: Vec<HistoryGroup> = Vec::new();
    for commit in commits {
        if let Some(group) = groups.last_mut() {
            let previous = group.commits.last();
            let should_merge = previous
                .map(|p| {
                    (p.authored_at_ms - commit.authored_at_ms).abs() <= HISTORY_GROUP_WINDOW_MS
                })
                .unwrap_or(false);
            if should_merge {
                group.oldest_authored_at_ms = commit.authored_at_ms;
                if !group.repo_names.contains(&commit.repo_name) {
                    group.repo_names.push(commit.repo_name.clone());
                }
                group.commits.push(commit);
                continue;
            }
        }
        groups.push(HistoryGroup {
            id: format!("{}-{}", commit.authored_at_ms, commit.hash),
            newest_authored_at_ms: commit.authored_at_ms,
            oldest_authored_at_ms: commit.authored_at_ms,
            authored_at_iso: commit.authored_at_iso.clone(),
            repo_names: vec![commit.repo_name.clone()],
            commits: vec![commit],
        });
    }
    groups
}

pub fn get_combined_history(project_path: &Path) -> Vec<HistoryGroup> {
    let mut histories = Vec::new();
    for entry in unique_repo_entries(project_path) {
        histories.extend(get_history_for_repo(&entry.path, &entry.name));
    }
    group_history_commits(histories)
}

/// Per-repo diffs for the commits of one history group.
pub fn get_history_group_diff(commits: &[HistoryCommitRef]) -> Vec<HistoryRepoDiff> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut by_repo: HashMap<(String, String), (String, Vec<String>)> = HashMap::new();
    for commit in commits {
        let key = (
            fsx::resolve(Path::new(&commit.repo_path))
                .display()
                .to_string(),
            commit.repo_name.clone(),
        );
        let entry = by_repo.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            (commit.repo_path.clone(), Vec::new())
        });
        entry.1.push(commit.hash.clone());
    }
    let mut result = Vec::new();
    for key in order {
        let (repo_path, hashes) = &by_repo[&key];
        let mut diffs = Vec::new();
        for hash in hashes {
            if let Ok(output) = git_output(
                Path::new(repo_path),
                &["show", "--format=", "--no-color", hash],
            ) {
                if output.status.success() {
                    let text = stdout_string(&output).trim().to_string();
                    if !text.is_empty() {
                        diffs.push(text);
                    }
                }
            }
        }
        if diffs.is_empty() {
            continue;
        }
        result.push(HistoryRepoDiff {
            repo_path: repo_path.clone(),
            repo_name: key.1,
            diff: diffs.join("\n"),
        });
    }
    result
}

fn doctor_result(repo_path: &Path) -> Option<DoctorResultPayload> {
    if !repo_path.join("multi.json").exists() {
        return None;
    }
    let report = run_doctor_checks(
        repo_path,
        &DoctorOptions {
            check_remotes: false,
        },
    );
    Some(DoctorResultPayload {
        errors: report.errors,
        warnings: report.warnings,
    })
}

#[derive(Debug, Clone)]
struct SummaryMetadata {
    last_commit_ms: Option<i64>,
    status: RepoStatus,
    doctor_result: Option<DoctorResultPayload>,
    sub_repos: Option<Vec<SubRepoPayload>>,
    branches: Vec<RepoBranchPayload>,
}

impl SummaryMetadata {
    fn fallback() -> Self {
        SummaryMetadata {
            last_commit_ms: None,
            status: RepoStatus::Unknown,
            doctor_result: None,
            sub_repos: None,
            branches: Vec::new(),
        }
    }
}

fn summary_metadata(project_path: &Path) -> SummaryMetadata {
    SummaryMetadata {
        last_commit_ms: get_project_last_commit_ms(project_path),
        status: get_project_status(project_path),
        doctor_result: doctor_result(project_path),
        sub_repos: subrepo_payloads(project_path),
        branches: get_project_branches(project_path),
    }
}

fn last_modified_ms(project_path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(project_path).ok()?.modified().ok()?;
    let since_epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(since_epoch.as_millis() as i64)
}

fn assemble_summary(project_path: &Path, metadata: SummaryMetadata) -> ProjectSummary {
    ProjectSummary {
        path: project_path.display().to_string(),
        name: project_name(project_path),
        last_modified_ms: last_modified_ms(project_path),
        last_commit_ms: metadata.last_commit_ms,
        status: metadata.status,
        doctor_result: metadata.doctor_result,
        sub_repos: metadata.sub_repos,
        branches: metadata.branches,
        worktrees: None,
    }
}

/// Full summary, computed synchronously.
pub fn get_project_summary(project_path: &Path) -> ProjectSummary {
    assemble_summary(project_path, summary_metadata(project_path))
}

struct SummaryCache {
    entries: HashMap<String, (Instant, SummaryMetadata)>,
    refreshing: HashSet<String>,
}

fn summary_cache() -> &'static Mutex<SummaryCache> {
    static CACHE: OnceLock<Mutex<SummaryCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(SummaryCache {
            entries: HashMap::new(),
            refreshing: HashSet::new(),
        })
    })
}

fn cached_metadata(path_key: &str) -> (Option<SummaryMetadata>, bool) {
    let cache = summary_cache().lock().unwrap_or_else(|e| e.into_inner());
    match cache.entries.get(path_key) {
        Some((cached_at, metadata)) => (
            Some(metadata.clone()),
            cached_at.elapsed() < METADATA_CACHE_TTL,
        ),
        None => (None, false),
    }
}

fn schedule_refresh(path_key: &str) {
    {
        let mut cache = summary_cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some((cached_at, _)) = cache.entries.get(path_key) {
            if cached_at.elapsed() < METADATA_CACHE_TTL {
                return;
            }
        }
        if !cache.refreshing.insert(path_key.to_string()) {
            return;
        }
    }
    let key = path_key.to_string();
    std::thread::spawn(move || {
        let metadata = summary_metadata(Path::new(&key));
        let mut cache = summary_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache
            .entries
            .insert(key.clone(), (Instant::now(), metadata));
        cache.refreshing.remove(&key);
    });
}

/// Fast summaries: served from the cache (possibly stale or a placeholder)
/// while a background refresh runs. Call [`refresh_projects_summary`] for
/// fresh data.
pub fn get_projects_summary(repo_paths: &[String]) -> Vec<ProjectSummary> {
    repo_paths
        .iter()
        .map(|path| {
            let (metadata, is_fresh) = cached_metadata(path);
            if !is_fresh {
                schedule_refresh(path);
            }
            assemble_summary(
                Path::new(path),
                metadata.unwrap_or_else(SummaryMetadata::fallback),
            )
        })
        .collect()
}

/// Recompute summaries in parallel, with an overall timeout. Projects that
/// do not finish in time get placeholder metadata.
pub fn refresh_projects_summary(repo_paths: &[String]) -> Vec<ProjectSummary> {
    let mut unique: Vec<String> = Vec::new();
    for path in repo_paths {
        if !path.is_empty() && !unique.contains(path) {
            unique.push(path.clone());
        }
    }
    let (tx, rx) = std::sync::mpsc::channel::<(String, SummaryMetadata)>();
    for path in &unique {
        let tx = tx.clone();
        let path = path.clone();
        std::thread::spawn(move || {
            let metadata = summary_metadata(Path::new(&path));
            let _ = tx.send((path, metadata));
        });
    }
    drop(tx);

    let deadline = Instant::now() + SUMMARY_TIMEOUT;
    let mut refreshed: HashMap<String, SummaryMetadata> = HashMap::new();
    while refreshed.len() < unique.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok((path, metadata)) => {
                let mut cache = summary_cache().lock().unwrap_or_else(|e| e.into_inner());
                cache
                    .entries
                    .insert(path.clone(), (Instant::now(), metadata.clone()));
                refreshed.insert(path, metadata);
            }
            Err(_) => break,
        }
    }
    unique
        .iter()
        .map(|path| {
            let metadata = refreshed
                .remove(path)
                .unwrap_or_else(SummaryMetadata::fallback);
            assemble_summary(Path::new(path), metadata)
        })
        .collect()
}

/// Everything the diff view needs for one project.
pub fn get_project_detail(project_path: &Path) -> ProjectDetail {
    let entries = unique_repo_entries(project_path);
    let mut repositories = Vec::new();
    let mut sync_states = BTreeMap::new();
    for entry in &entries {
        let path_str = entry.path.display().to_string();
        repositories.push(RepositoryDiff {
            path: path_str.clone(),
            name: entry.name.clone(),
            diff: get_repo_diff(&entry.path),
        });
        sync_states.insert(path_str, get_repo_sync_state(&entry.path));
    }
    ProjectDetail {
        repo_path: project_path.display().to_string(),
        repositories,
        history_groups: get_combined_history(project_path),
        sync_states_by_repo_path: sync_states,
        sub_repos: subrepo_payloads(project_path),
        branches: get_project_branches(project_path),
    }
}

/// Resolve a path to the repository that contains it: `(repo_path,
/// file_path_relative)`. Directories map to themselves.
pub fn resolve_navigation_path(input: &Path) -> Option<(PathBuf, Option<String>)> {
    let normalized = fsx::resolve(input);
    let meta = std::fs::metadata(&normalized).ok()?;
    if meta.is_dir() {
        return Some((normalized, None));
    }
    let parent = normalized.parent()?.to_path_buf();
    let repo_path = git_top_level(&parent).unwrap_or(parent);
    let file_path = normalized
        .strip_prefix(&repo_path)
        .ok()
        .map(|p| p.display().to_string())
        .filter(|p| !p.is_empty());
    Some((repo_path, file_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(hash: &str, ms: i64, repo: &str) -> HistoryCommit {
        HistoryCommit {
            hash: hash.into(),
            short_hash: hash[..3.min(hash.len())].into(),
            subject: "s".into(),
            description: String::new(),
            author_name: String::new(),
            author_email: String::new(),
            authored_at_ms: ms,
            authored_at_iso: iso_utc(ms),
            repo_path: format!("/{repo}"),
            repo_name: repo.into(),
        }
    }

    #[test]
    fn groups_commits_within_a_minute() {
        let groups = group_history_commits(vec![
            commit("aaa", 1_000, "a"),
            commit("bbb", 50_000, "b"),
            commit("ccc", 500_000, "a"),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].commits[0].hash, "ccc");
        assert_eq!(groups[1].repo_names, vec!["b".to_string(), "a".to_string()]);
        assert_eq!(groups[1].oldest_authored_at_ms, 1_000);
        assert_eq!(groups[1].newest_authored_at_ms, 50_000);
    }

    #[test]
    fn parses_history_records() {
        let record = "abc\x1fab\x1fAda\x1fada@x\x1f1700000000\x1fSubject\x1f\n  body line\nmore";
        let parsed = history_commit_from_record(record, Path::new("/r"), "r").unwrap();
        assert_eq!(parsed.authored_at_ms, 1_700_000_000_000);
        assert_eq!(parsed.description, "body line");
        assert_eq!(parsed.authored_at_iso, "2023-11-14T22:13:20Z");
        assert!(history_commit_from_record(
            "abc\x1fab\x1fAda\x1fada@x\x1fnotanumber\x1fS\x1f",
            Path::new("/r"),
            "r"
        )
        .is_none());
    }

    #[test]
    fn status_severity_ordering() {
        assert_eq!(
            worst_status(RepoStatus::Clean, RepoStatus::Dirty),
            RepoStatus::Dirty
        );
        assert_eq!(
            worst_status(RepoStatus::OutOfSync, RepoStatus::Unknown),
            RepoStatus::OutOfSync
        );
        assert_eq!(
            serde_json::to_string(&RepoStatus::OutOfSync).unwrap(),
            "\"out-of-sync\""
        );
    }
}
