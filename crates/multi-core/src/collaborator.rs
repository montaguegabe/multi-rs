//! `multi collaborator`: manage GitHub collaborators across every GitHub
//! repository in the workspace, via the `gh` CLI.

use std::path::PathBuf;
use std::process::Command;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::fsx;
use crate::git::{git_output, stdout_string};
use crate::paths::Paths;
use crate::prompt::{confirm, prompt};
use crate::repo_urls::parse_github_repo_slug;
use crate::repos::load_repos;

const RECENT_GITHUB_USERS_VERSION: u64 = 1;
const MAX_RECENT_GITHUB_USERS: usize = 50;

pub fn recent_github_users_file() -> PathBuf {
    fsx::multi_home_dir().join("recent-github-users.json")
}

fn dedupe_usernames(usernames: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::new();
    for username in usernames {
        let normalized = username.trim().to_string();
        if normalized.is_empty() {
            continue;
        }
        if seen.insert(normalized.to_lowercase()) {
            unique.push(normalized);
        }
    }
    unique
}

pub fn load_recent_github_usernames() -> Vec<String> {
    let path = recent_github_users_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            log::warn!("Failed to load recent GitHub usernames: {err}");
            return Vec::new();
        }
    };
    let data: Value = match serde_json::from_str(&text) {
        Ok(data) => data,
        Err(err) => {
            log::warn!("Failed to load recent GitHub usernames: {err}");
            return Vec::new();
        }
    };
    let users = match data {
        Value::Object(map) => map
            .get("users")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
        Value::Array(_) => data,
        _ => {
            log::warn!("Invalid recent GitHub usernames format, resetting");
            return Vec::new();
        }
    };
    let Value::Array(users) = users else {
        log::warn!("Invalid recent GitHub usernames list, resetting");
        return Vec::new();
    };
    dedupe_usernames(
        users
            .into_iter()
            .filter_map(|u| u.as_str().map(str::to_string)),
    )
}

fn save_recent_github_usernames(usernames: Vec<String>) {
    let users: Vec<String> = dedupe_usernames(usernames)
        .into_iter()
        .take(MAX_RECENT_GITHUB_USERS)
        .collect();
    let data = json!({"version": RECENT_GITHUB_USERS_VERSION, "users": users});
    let mut text = serde_json::to_string_pretty(&data).unwrap_or_default();
    text.push('\n');
    if let Err(err) = fsx::write(&recent_github_users_file(), text) {
        log::warn!("Failed to save recent GitHub usernames: {err}");
    }
}

fn record_recent_github_username(username: &str) {
    let normalized = username.trim();
    if normalized.is_empty() {
        return;
    }
    let mut users = vec![normalized.to_string()];
    users.extend(
        load_recent_github_usernames()
            .into_iter()
            .filter(|u| u.to_lowercase() != normalized.to_lowercase()),
    );
    save_recent_github_usernames(users);
}

/// Ask for a GitHub username, offering recent ones as a numbered list.
pub fn prompt_for_github_username() -> Result<String> {
    let recent = load_recent_github_usernames();
    if recent.is_empty() {
        loop {
            let username = prompt("GitHub username")?;
            if !username.is_empty() {
                return Ok(username);
            }
        }
    }
    println!("Recent GitHub users:");
    for (index, username) in recent.iter().enumerate() {
        println!("{}. {username}", index + 1);
    }
    loop {
        let selected = prompt("Select a GitHub username by number, or type a username")?;
        if let Ok(number) = selected.parse::<usize>() {
            if number >= 1 && number <= recent.len() {
                return Ok(recent[number - 1].clone());
            }
            println!("Choose a number from 1 to {}.", recent.len());
            continue;
        }
        if !selected.is_empty() {
            return Ok(selected);
        }
        println!("Enter a GitHub username or choose a number.");
    }
}

fn ensure_gh_available() -> Result<()> {
    let found = Command::new("gh").arg("--version").output().is_ok();
    if !found {
        return Err(Error::Other(
            "GitHub CLI `gh` is required for collaborator management.".to_string(),
        ));
    }
    Ok(())
}

struct GhApiRequest<'a> {
    method: &'a str,
    endpoint: &'a str,
    fields: &'a [(&'a str, &'a str)],
    paginate: bool,
    slurp: bool,
}

fn run_gh_api(request: &GhApiRequest) -> Result<String> {
    let mut cmd = Command::new("gh");
    cmd.args(["api", "--method", request.method, request.endpoint]);
    for (key, value) in request.fields {
        cmd.args(["-f", &format!("{key}={value}")]);
    }
    if request.paginate {
        cmd.arg("--paginate");
    }
    if request.slurp {
        cmd.arg("--slurp");
    }
    let output = cmd.output().map_err(|e| Error::Other(e.to_string()))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Err(Error::Other(if !stderr.is_empty() {
        stderr
    } else {
        stdout
    }))
}

fn load_pending_repository_invitations() -> Result<Vec<Value>> {
    let stdout = run_gh_api(&GhApiRequest {
        method: "GET",
        endpoint: "user/repository_invitations?per_page=100",
        fields: &[],
        paginate: true,
        slurp: true,
    })?;
    let text = if stdout.trim().is_empty() {
        "[]"
    } else {
        stdout.as_str()
    };
    let invitations: Value = serde_json::from_str(text).map_err(|_| {
        Error::Other("GitHub returned invalid repository invitation data.".to_string())
    })?;
    let Value::Array(items) = invitations else {
        return Err(Error::Other(
            "GitHub returned invalid repository invitation data.".to_string(),
        ));
    };
    if !items.is_empty() && items.iter().all(Value::is_array) {
        return Ok(items
            .into_iter()
            .flat_map(|page| page.as_array().cloned().unwrap_or_default())
            .filter(Value::is_object)
            .collect());
    }
    Ok(items.into_iter().filter(Value::is_object).collect())
}

fn workspace_github_repo_slug(paths: &Paths) -> Option<String> {
    let output = git_output(&paths.root_dir, &["remote", "get-url", "origin"]).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_github_repo_slug(stdout_string(&output).trim())
}

/// `(label, OWNER/REPO)` pairs for the workspace and every sub-repo.
pub fn load_github_repo_targets(paths: &Paths) -> Result<Vec<(String, String)>> {
    let mut targets: Vec<(String, String)> = Vec::new();
    let mut invalid: Vec<String> = Vec::new();
    if let Some(slug) = workspace_github_repo_slug(paths) {
        targets.push(("workspace".to_string(), slug));
    }
    for repo in load_repos(paths)? {
        match repo.url.as_deref().and_then(parse_github_repo_slug) {
            Some(slug) => targets.push((repo.name, slug)),
            None => invalid.push(repo.name),
        }
    }
    if !invalid.is_empty() {
        invalid.sort();
        return Err(Error::Other(format!(
            "Collaborator management currently supports only GitHub-hosted repos. Unsupported repos: {}",
            invalid.join(", ")
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<(String, String)> = targets
        .into_iter()
        .filter(|(_, slug)| seen.insert(slug.clone()))
        .collect();
    if unique.is_empty() {
        return Err(Error::Other(
            "No GitHub-hosted repositories were found in this workspace.".to_string(),
        ));
    }
    Ok(unique)
}

fn verify_user_exists(username: &str) -> Result<()> {
    run_gh_api(&GhApiRequest {
        method: "GET",
        endpoint: &format!("users/{username}"),
        fields: &[],
        paginate: false,
        slurp: false,
    })
    .map(|_| ())
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `add` or `remove`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollaboratorAction {
    Add,
    Remove,
}

impl CollaboratorAction {
    fn verb(self) -> &'static str {
        match self {
            CollaboratorAction::Add => "add",
            CollaboratorAction::Remove => "remove",
        }
    }
}

/// Add or remove `username` on every GitHub repo of the workspace.
pub fn manage_collaborator(
    paths: &Paths,
    action: CollaboratorAction,
    username: &str,
    permission: Option<&str>,
    yes: bool,
) -> Result<()> {
    ensure_gh_available()?;
    let targets = load_github_repo_targets(paths)?;
    verify_user_exists(username)?;
    if !yes {
        let repo_list: Vec<String> = targets
            .iter()
            .map(|(label, slug)| format!("- {label}: {slug}"))
            .collect();
        let question = format!(
            "{} collaborator '{username}' in these repos?\n{}",
            capitalize(action.verb()),
            repo_list.join("\n")
        );
        if !confirm(&question, false)? {
            return Err(Error::Other("Operation cancelled.".to_string()));
        }
    }
    record_recent_github_username(username);

    let mut failures: Vec<(String, String)> = Vec::new();
    for (_, slug) in &targets {
        let endpoint = format!("repos/{slug}/collaborators/{username}");
        let result = match action {
            CollaboratorAction::Add => {
                log::info!("Adding collaborator {username} to {slug}");
                run_gh_api(&GhApiRequest {
                    method: "PUT",
                    endpoint: &endpoint,
                    fields: &[("permission", permission.unwrap_or("push"))],
                    paginate: false,
                    slurp: false,
                })
            }
            CollaboratorAction::Remove => {
                log::info!("Removing collaborator {username} from {slug}");
                run_gh_api(&GhApiRequest {
                    method: "DELETE",
                    endpoint: &endpoint,
                    fields: &[],
                    paginate: false,
                    slurp: false,
                })
            }
        };
        if let Err(err) = result {
            let message = err.to_string();
            log::error!(
                "Failed {} collaborator {username} on {slug}: {message}",
                action.verb()
            );
            failures.push((slug.clone(), message));
        }
    }
    if !failures.is_empty() {
        let lines: Vec<String> = failures
            .iter()
            .map(|(slug, message)| format!("- {slug}: {message}"))
            .collect();
        return Err(Error::Other(format!(
            "Finished {} collaborator {username} with failures:\n{}",
            action.verb(),
            lines.join("\n")
        )));
    }
    log::info!(
        "✅ Finished {} collaborator {username} across all workspace repos",
        action.verb()
    );
    Ok(())
}

/// Accept pending invitations for every workspace repo as the current `gh`
/// user.
pub fn accept_collaborator_invitations(paths: &Paths, yes: bool) -> Result<()> {
    ensure_gh_available()?;
    let targets = load_github_repo_targets(paths)?;
    let target_slugs: std::collections::HashSet<String> = targets
        .iter()
        .map(|(_, slug)| slug.to_lowercase())
        .collect();

    let invitations: Vec<(String, i64)> = load_pending_repository_invitations()?
        .into_iter()
        .filter_map(|invitation| {
            let slug = invitation
                .get("repository")
                .and_then(|r| r.get("full_name"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())?
                .to_string();
            let id = invitation.get("id").and_then(Value::as_i64)?;
            target_slugs
                .contains(&slug.to_lowercase())
                .then_some((slug, id))
        })
        .collect();
    if invitations.is_empty() {
        println!("No pending repository invitations found for this workspace.");
        return Ok(());
    }
    if !yes {
        let repo_list: Vec<String> = invitations
            .iter()
            .map(|(slug, _)| format!("- {slug}"))
            .collect();
        let question = format!(
            "Accept repository invitations for these repos?\n{}",
            repo_list.join("\n")
        );
        if !confirm(&question, false)? {
            return Err(Error::Other("Operation cancelled.".to_string()));
        }
    }
    let mut failures: Vec<(String, String)> = Vec::new();
    for (slug, id) in &invitations {
        log::info!("Accepting repository invitation for {slug}");
        let result = run_gh_api(&GhApiRequest {
            method: "PATCH",
            endpoint: &format!("user/repository_invitations/{id}"),
            fields: &[],
            paginate: false,
            slurp: false,
        });
        if let Err(err) = result {
            let message = err.to_string();
            log::error!("Failed accepting repository invitation for {slug}: {message}");
            failures.push((slug.clone(), message));
        }
    }
    if !failures.is_empty() {
        let lines: Vec<String> = failures
            .iter()
            .map(|(slug, message)| format!("- {slug}: {message}"))
            .collect();
        return Err(Error::Other(format!(
            "Finished accepting repository invitations with failures:\n{}",
            lines.join("\n")
        )));
    }
    log::info!("✅ Accepted repository invitations for all workspace repos");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_case_insensitively() {
        let users = dedupe_usernames(vec![
            " Octocat ".into(),
            "octocat".into(),
            "".into(),
            "hub".into(),
        ]);
        assert_eq!(users, vec!["Octocat".to_string(), "hub".to_string()]);
    }
}
