//! Command definitions (clap) and dispatch into multi-core.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use multi_core::add_remove::{add_repo, remove_repo, RemoveOptions};
use multi_core::branch::{check_branch_alignment, set_branch_in_all_repos};
use multi_core::collaborator::{
    accept_collaborator_invitations, load_recent_github_usernames, manage_collaborator,
    prompt_for_github_username, CollaboratorAction,
};
use multi_core::convert_monorepo::convert_to_monorepo;
use multi_core::doctor::{log_report, run_doctor_checks, run_doctor_fixes, DoctorOptions};
use multi_core::error::{Error, Result};
use multi_core::git::check_all_on_same_branch;
use multi_core::git_run::run_git_in_all_repos;
use multi_core::hooks::{format_status, hooks_status, install_hooks, uninstall_hooks};
use multi_core::ignore_files::update_gitignore_with_generated_files;
use multi_core::init::{run_init, InitInputs};
use multi_core::sync::vscode::{
    merge_extensions_json, merge_launch_json, merge_settings_json, merge_tasks_json,
    merge_vscode_configs, sync_devcontainer,
};
use multi_core::sync::{agents, github, sync, SyncOptions};
use multi_core::worktree::{add_worktree, WorktreeAddOptions};
use multi_core::Paths;

/// Multi - manage multiple Git repositories as one VS Code / Cursor workspace.
///
/// Key features: synchronize git operations across the root and sub-repos,
/// merge .vscode configurations (launch.json, tasks.json, settings.json), and
/// keep consistent branch states across all repositories.
#[derive(Debug, Parser)]
#[command(name = "multi", version = multi_core::VERSION, about, long_about)]
pub struct Cli {
    /// Enable verbose output.
    #[arg(long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    pub fn install_set(&self) -> Option<&str> {
        match &self.command {
            Command::Sync(args) => args.install_set.as_deref(),
            Command::Worktree {
                command: WorktreeCommand::Add(args),
            } => args.install_set.as_deref(),
            _ => None,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Add a repository to the workspace.
    ///
    /// Appends the repo to multi.json and runs sync to clone it and merge
    /// all configurations. In monorepo mode, the repo's .git directory is
    /// removed after cloning so it becomes part of the root repository.
    Add {
        repo_url: String,
        /// Override directory name derived from URL.
        #[arg(long)]
        name: Option<String>,
    },
    /// Show branch state and expected-branch alignment.
    ///
    /// Running `multi branch` is the backwards-compatible spelling of
    /// `multi branch check`.
    Branch {
        #[command(subcommand)]
        command: Option<BranchCommand>,
    },
    /// Manage GitHub collaborators across all workspace repositories.
    Collaborator {
        #[command(subcommand)]
        command: CollaboratorCommand,
    },
    /// Convert a standard multi workspace to monorepo mode.
    ConvertMonorepo {
        /// Confirm conversion to monorepo mode (required).
        #[arg(long)]
        confirm: bool,
    },
    /// Diagnose common workspace configuration issues.
    Doctor {
        /// Fail on warnings in addition to errors.
        #[arg(long)]
        strict: bool,
        /// Output results as JSON.
        #[arg(long)]
        json: bool,
        /// Automatically apply safe fixes for tracked sub-repos in the root index.
        #[arg(long)]
        fix: bool,
        /// Skip the `git ls-remote` reachability check for declared remotes.
        #[arg(long)]
        no_remote_check: bool,
    },
    /// Run a git command across all repositories.
    ///
    /// Example: multi git pull / multi git checkout -b feature/x
    ///
    /// Requires all cloned repositories to be on their expected branch
    /// (working trees may be dirty). Repos not cloned in this workspace are
    /// skipped. To inspect branches when they mismatch, use `multi branch`.
    Git {
        /// The git command and arguments to run (e.g. 'pull' or 'checkout main').
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        git_args: Vec<String>,
    },
    /// Manage workspace-level git hooks (multi.json "hooks.path").
    ///
    /// One hooks directory at the workspace root governs the root repo and
    /// every sub-repo. `multi sync`, `multi init` and `multi worktree add`
    /// install it automatically.
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// Initialize a new multi workspace.
    ///
    /// Collects repository URLs interactively or from options, creates
    /// multi.json, runs sync (which initializes git and README if needed)
    /// and commits the changes.
    Init(InitArgs),
    /// Open a project in the Multi desktop app.
    Open { path: PathBuf },
    /// Remove a repository from the workspace.
    ///
    /// Removes the repo entry from multi.json and cleans up ignore files.
    Remove {
        repo_name: String,
        /// Also delete local directory/symlink.
        #[arg(long)]
        delete: bool,
        /// Skip uncommitted changes check.
        #[arg(long)]
        force: bool,
    },
    /// Run the Multi local JSON service (loopback only).
    Service {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    /// Create and switch to a branch in all repositories.
    SetBranch {
        /// Name of the branch to create and switch to.
        branch_name: String,
    },
    /// Sync development environment and configurations.
    ///
    /// Without a subcommand, performs a complete sync: clones missing
    /// repositories, installs hooks, merges VS Code configurations,
    /// generates agent files and syncs monorepo GitHub workflows.
    Sync(SyncArgs),
    /// Manage git worktrees for a multi workspace.
    ///
    /// Worktrees live in a sibling directory named after the workspace with
    /// '-worktrees' appended (e.g. my-workspace-worktrees/).
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
}

impl Command {
    /// Commands excluded from the post-command branch check: `branch`
    /// reports branch state itself, `doctor` runs outside workspaces,
    /// `hooks` and `recent-users` are branch-independent, and `open` /
    /// `service` are not workspace commands.
    pub fn runs_post_branch_check(&self) -> bool {
        !matches!(
            self,
            Command::Branch { .. }
                | Command::Doctor { .. }
                | Command::Hooks { .. }
                | Command::Open { .. }
                | Command::Service { .. }
                | Command::Collaborator {
                    command: CollaboratorCommand::RecentUsers
                }
        )
    }
}

#[derive(Debug, Subcommand)]
pub enum BranchCommand {
    /// Check that repos are on their expected branches.
    Check,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Permission {
    Pull,
    Push,
    Admin,
    Maintain,
    Triage,
}

impl Permission {
    fn as_str(self) -> &'static str {
        match self {
            Permission::Pull => "pull",
            Permission::Push => "push",
            Permission::Admin => "admin",
            Permission::Maintain => "maintain",
            Permission::Triage => "triage",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum CollaboratorCommand {
    /// Add a GitHub collaborator to every repo in this workspace.
    Add {
        username: Option<String>,
        /// Repository permission to grant.
        #[arg(long, value_enum, default_value_t = Permission::Push)]
        permission: Permission,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Accept pending collaborator invitations for this workspace.
    Accept {
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Remove a GitHub collaborator from every repo in this workspace.
    Remove {
        username: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// List recent GitHub usernames used by collaborator commands.
    RecentUsers,
}

#[derive(Debug, Subcommand)]
pub enum HooksCommand {
    /// Point core.hooksPath of every repo at the workspace hooks directory.
    Install,
    /// Report whether every repo uses the workspace hooks. Exits 1 if not.
    Status {
        /// Output JSON.
        #[arg(long)]
        json: bool,
    },
    /// Unset core.hooksPath wherever it points at the workspace hooks.
    Uninstall,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Visibility {
    Private,
    Public,
    Internal,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CloneProtocol {
    Https,
    Ssh,
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Repository URL to include. Repeat to initialize non-interactively.
    #[arg(long = "repo")]
    repo_urls: Vec<String>,
    /// Description for the corresponding --repo. Repeat in the same order.
    #[arg(long = "repo-description")]
    repo_descriptions: Vec<String>,
    /// GitHub repository to create via gh in OWNER/REPO format.
    #[arg(long = "github-repo")]
    github_repos: Vec<String>,
    /// Description for the corresponding --github-repo. Also passed to gh repo create.
    #[arg(long = "github-description")]
    github_descriptions: Vec<String>,
    /// Visibility to use for each --github-repo.
    #[arg(long, value_enum, default_value_t = Visibility::Private)]
    github_visibility: Visibility,
    /// Clone URL format written to multi.json for created GitHub repos.
    #[arg(long, value_enum, default_value_t = CloneProtocol::Https)]
    github_clone_protocol: CloneProtocol,
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Only sync repositories included in the named install set.
    #[arg(long = "install-set", visible_alias = "set", value_name = "NAME")]
    pub install_set: Option<String>,
    #[command(subcommand)]
    pub command: Option<SyncCommand>,
}

#[derive(Debug, Subcommand)]
pub enum SyncCommand {
    /// Manage VS Code configuration files across repositories.
    ///
    /// Without a subcommand, merges all (settings, launch, tasks, extensions)
    /// and syncs the .devcontainer folder.
    Vscode {
        #[command(subcommand)]
        command: Option<VscodeCommand>,
    },
    /// Generate AGENTS.md and CLAUDE.md from AGENTS.parts/*.md files.
    Agents,
    /// Sync GitHub Actions workflow files into root .github/workflows (monorepo mode only).
    Github,
}

#[derive(Debug, Subcommand)]
pub enum VscodeCommand {
    /// Merge launch.json files from all repositories into the root .vscode directory.
    Launch,
    /// Merge settings.json files from all repositories into the root .vscode directory.
    Settings,
    /// Merge tasks.json files from all repositories into the root .vscode directory.
    Tasks,
    /// Merge extensions.json files from all repositories into the root .vscode directory.
    Extensions,
    /// Copy .devcontainer folder from a sub-repo to the root workspace.
    Devcontainer,
}

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// Create an isolated worktree of the whole workspace.
    ///
    /// The worktree is created at <parent>/<workspace-dirname>-worktrees/NAME
    /// (the -worktrees directory is created if missing). NAME is used as the
    /// branch name when --branch is omitted.
    Add(WorktreeAddArgs),
}

#[derive(Debug, Args)]
pub struct WorktreeAddArgs {
    /// Directory name for the worktree.
    name: String,
    /// Branch name for the worktree. Defaults to NAME.
    #[arg(long = "branch")]
    branch_name: Option<String>,
    /// Only sync repositories included in the named install set.
    #[arg(long = "install-set", visible_alias = "set", value_name = "NAME")]
    install_set: Option<String>,
    /// Base ref for newly created root and sub-repo branches.
    #[arg(long, default_value = "HEAD")]
    base_ref: String,
}

fn cwd() -> Result<PathBuf> {
    Ok(std::env::current_dir()?)
}

fn workspace(install_set: Option<&str>) -> Result<Paths> {
    Paths::discover(cwd()?, install_set)
}

pub fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Add { repo_url, name } => {
            add_repo(&cwd()?, repo_url, name.as_deref())?;
        }
        Command::Branch { .. } => {
            check_branch_alignment(&workspace(None)?)?;
        }
        Command::Collaborator { command } => run_collaborator(command)?,
        Command::ConvertMonorepo { confirm } => {
            if !confirm {
                return Err(Error::Usage(
                    "This command is destructive and requires --confirm. It will delete nested .git directories in configured repos."
                        .to_string(),
                ));
            }
            convert_to_monorepo(&cwd()?)?;
        }
        Command::Doctor {
            strict,
            json,
            fix,
            no_remote_check,
        } => run_doctor(*strict, *json, *fix, !*no_remote_check)?,
        Command::Git { git_args } => {
            run_git_in_all_repos(&workspace(None)?, git_args)?;
        }
        Command::Hooks { command } => run_hooks(command)?,
        Command::Init(args) => {
            let inputs = InitInputs {
                repo_urls: args.repo_urls.clone(),
                repo_descriptions: args.repo_descriptions.clone(),
                github_repos: args.github_repos.clone(),
                github_descriptions: args.github_descriptions.clone(),
                github_visibility: format!("{:?}", args.github_visibility).to_lowercase(),
                github_clone_protocol: format!("{:?}", args.github_clone_protocol).to_lowercase(),
            };
            run_init(&cwd()?, &inputs)?;
        }
        Command::Open { path } => run_open(path)?,
        Command::Remove {
            repo_name,
            delete,
            force,
        } => {
            remove_repo(
                &cwd()?,
                repo_name,
                &RemoveOptions {
                    delete: *delete,
                    force: *force,
                },
            )?;
        }
        Command::Service { host, port } => multi_core::service::run(host, *port)?,
        Command::SetBranch { branch_name } => set_branch_in_all_repos(&cwd()?, branch_name)?,
        Command::Sync(args) => run_sync(args)?,
        Command::Worktree { command } => match command {
            WorktreeCommand::Add(args) => {
                add_worktree(
                    &cwd()?,
                    &args.name,
                    &WorktreeAddOptions {
                        branch_name: args.branch_name.clone(),
                        install_set: args.install_set.clone(),
                        base_ref: Some(args.base_ref.clone()),
                    },
                )?;
            }
        },
    }
    Ok(())
}

fn run_sync(args: &SyncArgs) -> Result<()> {
    let install_set = args.install_set.as_deref();
    let root = cwd()?;
    match &args.command {
        None => sync(
            &root,
            &SyncOptions {
                ensure_on_same_branch: true,
                install_set: args.install_set.clone(),
            },
        ),
        Some(SyncCommand::Vscode { command }) => {
            let paths = workspace(install_set)?;
            match command {
                None => merge_vscode_configs(&paths),
                Some(VscodeCommand::Launch) => {
                    log::info!("Merging launch.json files from all repositories...");
                    merge_launch_json(&paths)
                }
                Some(VscodeCommand::Settings) => {
                    log::info!("Merging settings.json files from all repositories...");
                    merge_settings_json(&paths)
                }
                Some(VscodeCommand::Tasks) => {
                    log::info!("Merging tasks.json files from all repositories...");
                    merge_tasks_json(&paths)
                }
                Some(VscodeCommand::Extensions) => {
                    log::info!("Merging extensions.json files from all repositories...");
                    merge_extensions_json(&paths)
                }
                Some(VscodeCommand::Devcontainer) => {
                    log::info!("Syncing .devcontainer folder...");
                    sync_devcontainer(&paths)
                }
            }
        }
        Some(SyncCommand::Agents) => {
            log::info!("Syncing AGENTS files...");
            let paths = workspace(install_set)?;
            update_gitignore_with_generated_files(&paths)?;
            agents::sync_all_agents(&paths)
        }
        Some(SyncCommand::Github) => {
            let paths = workspace(install_set)?;
            if !paths.settings()?.is_monorepo() {
                return Err(Error::Usage(
                    "The 'multi sync github' command is only available in monorepo mode."
                        .to_string(),
                ));
            }
            github::sync_all_github_actions(&paths)
        }
    }
}

fn run_doctor(strict: bool, json: bool, fix: bool, check_remotes: bool) -> Result<()> {
    let target = cwd()?;
    let options = DoctorOptions { check_remotes };
    let mut report = run_doctor_checks(&target, &options);
    if fix && report.errors.is_empty() {
        let fixed = run_doctor_fixes(&target)?;
        if fixed.is_empty() {
            report
                .infos
                .push("No tracked sub-repos were found in the root git index to fix.".to_string());
        } else {
            report = run_doctor_checks(&target, &options);
            report.infos.push(format!(
                "Applied --fix: removed tracked sub-repos from the root git index: {}.",
                fixed.join(", ")
            ));
        }
    }
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        log_report(&report);
    }
    if report.should_fail(strict) {
        return Err(Error::Other("Doctor checks failed.".to_string()));
    }
    Ok(())
}

fn run_hooks(command: &HooksCommand) -> Result<()> {
    let paths = workspace(None)?;
    match command {
        HooksCommand::Install => {
            if multi_core::hooks::configured_hooks_path(&paths)?.is_none() {
                return Err(Error::Usage(
                    "No workspace hooks configured. Add \"hooks\": {\"path\": \".githooks\"} to multi.json."
                        .to_string(),
                ));
            }
            install_hooks(&paths)?;
            let status = hooks_status(&paths)?;
            print!("{}", format_status(&status));
            if !status.ok {
                return Err(Error::Other(
                    "Workspace hooks are not fully installed.".to_string(),
                ));
            }
        }
        HooksCommand::Status { json } => {
            let status = hooks_status(&paths)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!("{}", format_status(&status));
            }
            if !status.ok {
                std::process::exit(1);
            }
        }
        HooksCommand::Uninstall => {
            let removed = uninstall_hooks(&paths)?;
            let list = if removed.is_empty() {
                "(none)".to_string()
            } else {
                removed.join(", ")
            };
            println!("Removed workspace hooks from: {list}");
        }
    }
    Ok(())
}

fn run_collaborator(command: &CollaboratorCommand) -> Result<()> {
    match command {
        CollaboratorCommand::Add {
            username,
            permission,
            yes,
        } => {
            let username = match username {
                Some(username) => username.clone(),
                None => prompt_for_github_username()?,
            };
            manage_collaborator(
                &workspace(None)?,
                CollaboratorAction::Add,
                &username,
                Some(permission.as_str()),
                *yes,
            )
        }
        CollaboratorCommand::Accept { yes } => {
            accept_collaborator_invitations(&workspace(None)?, *yes)
        }
        CollaboratorCommand::Remove { username, yes } => manage_collaborator(
            &workspace(None)?,
            CollaboratorAction::Remove,
            username,
            None,
            *yes,
        ),
        CollaboratorCommand::RecentUsers => {
            let users = load_recent_github_usernames();
            if users.is_empty() {
                println!("No recent GitHub users.");
            }
            for user in users {
                println!("{user}");
            }
            Ok(())
        }
    }
}

fn run_open(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(Error::Usage(format!(
            "Path '{}' does not exist.",
            path.display()
        )));
    }
    let absolute = multi_core::fsx::resolve(path);
    let encoded = percent_encoding::utf8_percent_encode(
        &absolute.display().to_string(),
        percent_encoding::NON_ALPHANUMERIC,
    )
    .to_string();
    let url = format!("multi://open?path={encoded}");
    open::that(&url).map_err(|e| Error::Other(format!("Failed to open {url}: {e}")))
}

/// Verify expected-branch alignment after a command (never fatal).
pub fn post_command_branch_check(install_set: Option<&str>) -> Result<()> {
    let paths = match Paths::from_cwd(install_set) {
        Ok(paths) => paths,
        // Commands that run outside a multi workspace have nothing to check.
        Err(Error::NotAWorkspace) => return Ok(()),
        Err(err) => return Err(err),
    };
    if paths.settings()?.is_monorepo() {
        return Ok(());
    }
    check_all_on_same_branch(&paths, true).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn git_args_pass_through_options() {
        let cli = Cli::parse_from(["multi", "git", "branch", "--show-current"]);
        match cli.command {
            Command::Git { git_args } => assert_eq!(git_args, vec!["branch", "--show-current"]),
            _ => panic!("expected git"),
        }
    }

    #[test]
    fn sync_accepts_install_set_alias_before_subcommand() {
        let cli = Cli::parse_from(["multi", "sync", "--set", "default", "vscode", "settings"]);
        assert_eq!(cli.install_set(), Some("default"));
        match cli.command {
            Command::Sync(args) => assert!(matches!(
                args.command,
                Some(SyncCommand::Vscode {
                    command: Some(VscodeCommand::Settings)
                })
            )),
            _ => panic!("expected sync"),
        }
    }
}
