//! L1 analysis: systems from workspace manifests (REQUIREMENTS §3.1).
//!
//! Analysers read repository files through a path list and a `read`
//! callback, so the same code runs on a git tree, a directory or test
//! fixtures. Nothing in the repository is ever executed (§5.5).

pub mod cargo;

use blueprint_model::RepoPath;

/// Something an analyser could not fully understand. The run continues; the
/// affected node is degraded instead (§5.5).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Diagnostic {
    /// The file the problem is in.
    pub path: RepoPath,
    /// What went wrong.
    pub message: String,
}

/// Reads a file's bytes by repository path, or `None` if it cannot be read.
pub type Read<'a> = &'a dyn Fn(&RepoPath) -> Option<Vec<u8>>;
