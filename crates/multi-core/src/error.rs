use std::path::Path;

/// Errors raised by multi-core.
///
/// The variants mirror the exception classes of the original Python
/// implementation so callers (the CLI, the desktop app) can keep the same
/// control flow: configuration problems, git failures, dirty working trees,
/// an empty repo list, and usage errors that should exit with status 2.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid `multi.json` content or repo configuration.
    #[error("{0}")]
    Config(String),
    /// A git command failed or a repository is in an unexpected state.
    #[error("{0}")]
    Git(String),
    /// A working tree has uncommitted changes where a clean tree is required.
    #[error("{0}")]
    RepoNotClean(String),
    /// `multi.json` declares no repositories (for the selected install set).
    #[error("{0}")]
    NoRepositories(String),
    /// The command was invoked in a way that is not supported.
    #[error("{0}")]
    Usage(String),
    /// No `multi.json` was found in the target directory or any parent.
    #[error("Could not find multi.json in any parent directory")]
    NotAWorkspace,
    /// Any other failure with a human-readable message.
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Other(err.to_string())
    }
}

impl Error {
    /// Wrap an I/O error with the path it concerned.
    pub fn io(path: &Path, err: std::io::Error) -> Self {
        Error::Other(format!("{}: {}", path.display(), err))
    }
}
