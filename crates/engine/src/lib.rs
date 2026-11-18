//! The headless engine (REQUIREMENTS §5.3): indexing, incremental queries,
//! git, storage and diffing. It has no UI dependency and everything it can
//! show is reachable from the CLI.

pub mod git;
pub mod index;
pub mod l1;
pub mod l2;
pub mod l34;
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

/// Builds the full L1–L4 model from a set of files: [`analyse`], then every
/// language analyser over the files (DR 0008). Never fails.
pub fn analyse_deep(paths: &[RepoPath], read: l1::Read<'_>) -> (Model, Vec<l1::Diagnostic>) {
    analyse_deep_cached(paths, read, &|_| None, &mut |_, _| {})
}

/// As [`analyse_deep`], but takes each Rust file's facts from `cached` when
/// it has them (see [`l34::analyse_cached`]); `parsed` sees every file that
/// had to be parsed.
pub fn analyse_deep_cached(
    paths: &[RepoPath],
    read: l1::Read<'_>,
    cached: l34::Cached<'_>,
    parsed: &mut dyn FnMut(&RepoPath, &blueprint_model::lang::FileFacts),
) -> (Model, Vec<l1::Diagnostic>) {
    let mut model = Model::default();
    let mut diags = l1::analyse(paths, read, &mut model);
    let (more, placed) = l2::analyse_placed(paths, read, &mut model);
    diags.extend(more);
    diags.extend(l34::analyse_cached(
        &placed, read, cached, parsed, &mut model,
    ));
    diags.sort();
    (model, diags)
}
