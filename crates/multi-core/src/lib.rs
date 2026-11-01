//! Core library behind the `multi` CLI and the Multi desktop app.
//!
//! A multi workspace is a directory with a `multi.json` listing sub-repos that
//! are cloned (not submoduled) underneath it. This crate implements workspace
//! discovery, syncing (clone, hooks, merged `.vscode` config, generated agent
//! files, monorepo GitHub workflows), branch orchestration, worktrees,
//! diagnostics, GitHub collaborator management, and the read-only project
//! inspection API the desktop app renders.

pub mod add_remove;
pub mod app_api;
pub mod bootstrap;
pub mod branch;
pub mod collaborator;
pub mod convert_monorepo;
pub mod doctor;
pub mod error;
pub mod fsx;
pub mod git;
pub mod git_run;
pub mod hooks;
pub mod ignore_files;
pub mod init;
pub mod jsonc;
pub mod logging;
pub mod managed_blocks;
pub mod paths;
pub mod prompt;
pub mod registry;
pub mod repo_urls;
pub mod repos;
pub mod service;
pub mod settings;
pub mod skills;
pub mod sync;
pub mod worktree;

pub use error::{Error, Result};
pub use paths::Paths;
pub use repos::{load_cloned_repos, load_repos, Repository};
pub use settings::Settings;

/// Crate version, reported by `multi --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
