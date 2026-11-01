//! Install the Multi agent skills.
//!
//! `multi skills install` copies the published skills with the skills.sh
//! installer (`npx skills add`). With `dev` it instead runs the skills repo's
//! own `scripts/install-dev.sh`, which symlinks the checked-out skills into
//! `~/.agents/skills` and `~/.claude/skills` so edits are live for Codex and
//! Claude Code. The symlink logic lives in that script, shared by every skills
//! repo made from the BoilerSync `skills` template; this module only finds the
//! repo and runs it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// GitHub `owner/repo` of the published skills, as `npx skills add` expects.
pub const SKILLS_REPO: &str = "montaguegabe/multi-skills";
/// Directory name of the skills repo checkout inside the multi workspace.
pub const SKILLS_REPO_DIRNAME: &str = "multi-skills";
/// Skills installed by default without `dev`.
pub const PUBLISHED_SKILLS: &[&str] = &["multi-workspace"];
/// Agents `npx skills add` installs for.
pub const SKILLS_AGENTS: &[&str] = &["codex", "claude-code"];
const DEV_INSTALL_SCRIPT: &str = "scripts/install-dev.sh";

/// How to install: a symlinked development checkout, or published copies.
#[derive(Debug, Clone, Default)]
pub struct SkillsInstallOptions {
    /// Symlink a local checkout instead of copying the published skills.
    pub dev: bool,
    /// Skills repo checkout to link with `dev`. Found automatically when `None`.
    pub source: Option<PathBuf>,
    /// Skill names to install. Empty means every skill.
    pub skills: Vec<String>,
}

fn is_skills_repo(path: &Path) -> bool {
    path.join("skills").is_dir()
}

/// Return the local skills repo checkout.
///
/// An explicit `source` wins. Otherwise look for a `multi-skills` directory
/// next to `cwd` or any of its parents, which is where the multi workspace
/// keeps it.
pub fn locate_skills_repo(source: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    if let Some(source) = source {
        if !is_skills_repo(source) {
            return Err(Error::Usage(format!(
                "{} has no skills/ directory.",
                source.display()
            )));
        }
        return Ok(crate::fsx::resolve(source));
    }

    let start = crate::fsx::resolve(cwd);
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(SKILLS_REPO_DIRNAME);
        if is_skills_repo(&candidate) {
            return Ok(crate::fsx::resolve(&candidate));
        }
    }

    Err(Error::Usage(format!(
        "Could not find a {SKILLS_REPO_DIRNAME} checkout next to {} or any parent directory. Pass --source PATH.",
        start.display()
    )))
}

/// The `scripts/install-dev.sh` invocation for `repo`, linking `skills` (or
/// every skill when empty).
pub fn dev_install_command(repo: &Path, skills: &[String]) -> Result<Vec<String>> {
    let script = repo.join(DEV_INSTALL_SCRIPT);
    if !script.is_file() {
        return Err(Error::Usage(format!(
            "{} is missing. Pull the latest skills template into {} (it ships {DEV_INSTALL_SCRIPT}) and retry.",
            script.display(),
            repo.display()
        )));
    }
    let mut command = vec![script.display().to_string()];
    command.extend(skills.iter().cloned());
    Ok(command)
}

/// One `npx skills add` invocation per published skill, for both agents.
pub fn published_install_commands(skills: &[String]) -> Vec<Vec<String>> {
    skills
        .iter()
        .map(|skill| {
            let mut command: Vec<String> = ["npx", "-y", "skills", "add", SKILLS_REPO, "--skill"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            command.push(skill.clone());
            command.push("-g".to_string());
            for agent in SKILLS_AGENTS {
                command.push("-a".to_string());
                command.push(agent.to_string());
            }
            command.push("-y".to_string());
            command
        })
        .collect()
}

fn run(command: &[String]) -> Result<()> {
    let status = Command::new(&command[0])
        .args(&command[1..])
        .status()
        .map_err(|e| Error::Other(format!("Failed to run {}: {e}", command[0])))?;
    if !status.success() {
        return Err(Error::Other(format!(
            "{} exited with {status}",
            command.join(" ")
        )));
    }
    Ok(())
}

/// Install the skills per `options`, resolving the dev checkout from `cwd`.
pub fn install_skills(options: &SkillsInstallOptions, cwd: &Path) -> Result<()> {
    if options.dev {
        let repo = locate_skills_repo(options.source.as_deref(), cwd)?;
        log::info!("Linking skills from {}", repo.display());
        return run(&dev_install_command(&repo, &options.skills)?);
    }

    let skills: Vec<String> = if options.skills.is_empty() {
        PUBLISHED_SKILLS.iter().map(|s| s.to_string()).collect()
    } else {
        options.skills.clone()
    };
    for command in published_install_commands(&skills) {
        log::info!("Running {}", command.join(" "));
        run(&command)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skills_repo(root: &Path, with_script: bool) -> PathBuf {
        let repo = root.join(SKILLS_REPO_DIRNAME);
        let skill = repo.join("skills").join("multi-workspace");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: multi-workspace\n---\n").unwrap();
        if with_script {
            let script = repo.join(DEV_INSTALL_SCRIPT);
            std::fs::create_dir_all(script.parent().unwrap()).unwrap();
            std::fs::write(
                &script,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$REPO_MARKER\"\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        repo
    }

    #[test]
    fn locate_uses_explicit_source() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_skills_repo(tmp.path(), true);
        assert_eq!(
            locate_skills_repo(Some(&repo), tmp.path()).unwrap(),
            crate::fsx::resolve(&repo)
        );
    }

    #[test]
    fn locate_rejects_source_without_skills_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            locate_skills_repo(Some(tmp.path()), tmp.path()),
            Err(Error::Usage(_))
        ));
    }

    #[test]
    fn locate_walks_up_from_cwd_to_workspace_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_skills_repo(tmp.path(), true);
        let nested = tmp.path().join("multi-rs").join("crates").join("multi-cli");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            locate_skills_repo(None, &nested).unwrap(),
            crate::fsx::resolve(&repo)
        );
    }

    #[test]
    fn locate_fails_clearly_when_nothing_found() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        match locate_skills_repo(None, &elsewhere) {
            Err(Error::Usage(message)) => assert!(message.contains("--source")),
            other => panic!("expected usage error, got {other:?}"),
        }
    }

    #[test]
    fn dev_install_command_requires_script() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_skills_repo(tmp.path(), false);
        match dev_install_command(&repo, &[]) {
            Err(Error::Usage(message)) => assert!(message.contains("install-dev.sh")),
            other => panic!("expected usage error, got {other:?}"),
        }
    }

    #[test]
    fn dev_install_command_passes_skill_names() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_skills_repo(tmp.path(), true);
        let command = dev_install_command(&repo, &["multi-workspace".to_string()]).unwrap();
        assert_eq!(
            command,
            vec![
                repo.join(DEV_INSTALL_SCRIPT).display().to_string(),
                "multi-workspace".to_string()
            ]
        );
    }

    #[test]
    fn published_install_commands_target_both_agents() {
        let skills: Vec<String> = PUBLISHED_SKILLS.iter().map(|s| s.to_string()).collect();
        let commands = published_install_commands(&skills);
        assert_eq!(commands.len(), PUBLISHED_SKILLS.len());
        for (command, skill) in commands.iter().zip(PUBLISHED_SKILLS) {
            assert_eq!(&command[..5], &["npx", "-y", "skills", "add", SKILLS_REPO]);
            let at = command.iter().position(|s| s == "--skill").unwrap();
            assert_eq!(command[at + 1], *skill);
            assert!(command.contains(&"-g".to_string()));
            assert_eq!(command.iter().filter(|s| *s == "-a").count(), 2);
        }
    }

    #[cfg(unix)]
    #[test]
    fn dev_install_runs_repo_script() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_skills_repo(tmp.path(), true);
        let marker = tmp.path().join("marker");
        std::env::set_var("REPO_MARKER", &marker);
        install_skills(
            &SkillsInstallOptions {
                dev: true,
                source: Some(repo),
                skills: vec!["multi-workspace".to_string()],
            },
            tmp.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(marker).unwrap().trim(),
            "multi-workspace"
        );
    }
}
