//! Git CLI write operations, safety guards, and repository file watcher.

pub mod cli;
pub mod safety;
pub mod watch;
pub mod write;

pub use cli::{CommandOutput, GitCli, OpsError, Result};
pub use safety::{SafetyRisk, SafetyWarning, check_checkout_safety, check_reset_safety};
pub use watch::{RepoWatcher, WatchEvent};
pub use write::ResetMode;
