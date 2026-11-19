use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

/// Convenience alias for results returned by `git-graph-ops`.
pub type Result<T> = std::result::Result<T, OpsError>;

/// Errors returned by Git CLI operations, safety checks, and repository watching.
#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    /// Could not locate a usable `git` executable.
    #[error("git executable not found: {0}")]
    GitNotFound(String),

    /// A `git` invocation exited with a non-zero status code.
    #[error("git `{}` failed (exit code {code:?}): {stderr}", args.join(" "))]
    CommandFailed {
        args: Vec<String>,
        code: Option<i32>,
        stderr: String,
    },

    /// Caller provided an empty or unsafe argument (e.g. leading `-`).
    #[error("invalid argument: {0}")]
    InvalidInput(String),

    /// Underlying filesystem or process spawning error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Filesystem watcher setup or runtime error.
    #[error("watcher error: {0}")]
    Watcher(#[from] notify::Error),
}

/// Captured output from a successful `git` CLI invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Thin synchronous wrapper around the system `git` executable.
///
/// Designed to be constructed once and shared across background worker threads
/// (e.g. GPUI's `background_spawn`).
#[derive(Debug, Clone)]
pub struct GitCli {
    git_path: PathBuf,
}

impl GitCli {
    /// Locates `git` on `PATH` using `which`.
    pub fn new() -> Result<Self> {
        let git_path = which::which("git").map_err(|err| OpsError::GitNotFound(err.to_string()))?;
        Ok(Self { git_path })
    }

    /// Constructs a `GitCli` from an explicit executable path.
    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self> {
        let git_path = path.into();
        if !git_path.is_file() {
            return Err(OpsError::GitNotFound(format!(
                "path does not exist or is not a file: {}",
                git_path.display()
            )));
        }
        Ok(Self { git_path })
    }

    /// Returns the resolved path to the `git` binary.
    pub fn git_path(&self) -> &Path {
        &self.git_path
    }

    /// Runs `git` synchronously in `repo`, returning stdout/stderr on exit 0
    /// or `OpsError::CommandFailed` otherwise.
    pub fn run<I, S>(&self, repo: impl AsRef<Path>, args: I) -> Result<CommandOutput>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let repo = repo.as_ref();
        let arg_strings: Vec<String> = args
            .into_iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();

        log::debug!("running git in {}: {:?}", repo.display(), arg_strings);

        let output = Command::new(&self.git_path)
            .current_dir(repo)
            .args(&arg_strings)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .stdin(Stdio::null())
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let code = output.status.code();

        if output.status.success() {
            return Ok(CommandOutput {
                stdout,
                stderr,
                code: code.unwrap_or(0),
            });
        }

        let err_msg = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };

        log::warn!(
            "git {:?} failed (code {:?}): {}",
            arg_strings,
            code,
            err_msg
        );

        Err(OpsError::CommandFailed {
            args: arg_strings,
            code,
            stderr: err_msg,
        })
    }

    /// Runs `git` and returns raw exit status + stdout without treating non-zero
    /// exit codes as an error (useful for probe commands like `symbolic-ref`).
    pub(crate) fn probe<I, S>(
        &self,
        repo: impl AsRef<Path>,
        args: I,
    ) -> Result<(ExitStatus, String)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new(&self.git_path)
            .current_dir(repo.as_ref())
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .stdin(Stdio::null())
            .output()?;

        Ok((
            output.status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }
}

/// Validates a non-empty ref/commit/remote argument that cannot be mistaken for a CLI flag.
pub(crate) fn validate_non_flag(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(OpsError::InvalidInput(format!("{label} cannot be empty")));
    }
    if value.starts_with('-') {
        return Err(OpsError::InvalidInput(format!(
            "{label} cannot start with '-': {value}"
        )));
    }
    Ok(())
}
