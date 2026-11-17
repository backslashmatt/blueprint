//! The headless engine (REQUIREMENTS §5.3): indexing, incremental queries,
//! git, storage and diffing. It has no UI dependency and everything it can
//! show is reachable from the CLI.

pub mod git;
pub mod l1;
pub mod l2;
pub mod source;

use blueprint_model::{Model, RepoPath};

/// Builds the L1–L2 model from a set of files: every L1 analyser, then L2.
/// Never fails; what could not be understood is in the diagnostics.
pub fn analyse(paths: &[RepoPath], read: l1::Read<'_>) -> (Model, Vec<l1::Diagnostic>) {
    let mut model = Model::default();
    let mut diags = l1::analyse(paths, read, &mut model);
    diags.extend(l2::analyse(paths, read, &mut model));
    diags.sort();
    (model, diags)
}
