//! L2 analysis: modules inside each system (REQUIREMENTS §3.1: "a package,
//! namespace or directory with a public surface").
//!
//! - **Rust:** the module tree of each Cargo package, from `src/lib.rs` and
//!   `src/main.rs`, following `mod` declarations ([`rust`]).
//! - **No analyser:** files no analyser reached (a system with no Rust
//!   crate root, or a crate's tests, benches, build script and docs) are
//!   shown by top-level directory, with `unknown` confidence (§3.4).
//! - **Unowned:** files outside every system go under one synthetic
//!   `unowned` system, laid out by directory, so nothing disappears.
//!
//! Every L2 node's parent is its L1 system; nesting is in the node's path.

pub mod rust;

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, Ecosystem, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey,
};

use crate::l1::{Diagnostic, Read};

/// The display name of the synthetic system for files outside every system.
pub const UNOWNED_NAME: &str = "(unowned)";

/// Adds L2 nodes for every L1 system already in `model`, and the unowned
/// system for files outside all of them. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let systems: Vec<(NodeId, SystemKey)> = model
        .level(Level::System)
        .map(|n| (n.id.clone(), n.id.system_key().clone()))
        .collect();

    // Each file belongs to the system with the deepest enclosing directory.
    // Several ecosystems can share a directory; the first (in key order) owns
    // its files. Looking up each ancestor keeps this linear in path depth.
    let mut by_dir: BTreeMap<&[String], &SystemKey> = BTreeMap::new();
    for (_, k) in &systems {
        by_dir.entry(k.dir.segments()).or_insert(k);
    }
    let mut owned: BTreeMap<SystemKey, Vec<&RepoPath>> = BTreeMap::new();
    let mut unowned: Vec<&RepoPath> = Vec::new();
    for p in paths {
        let segs = p.segments();
        let owner = (0..segs.len()).rev().find_map(|n| by_dir.get(&segs[..n]));
        match owner {
            Some(k) => owned.entry((*k).clone()).or_default().push(p),
            None => unowned.push(p),
        }
    }

    for (id, key) in &systems {
        let files: BTreeSet<&RepoPath> = owned.get(key).into_iter().flatten().copied().collect();
        let reached = if key.ecosystem == Ecosystem::Cargo {
            rust::analyse_crate(id, key, &files, read, model, &mut diags)
        } else {
            None
        };
        // Whatever the module tree did not reach (tests, benches, examples,
        // build scripts, docs, files behind unparsed macros) still shows up,
        // by directory.
        let rest: BTreeSet<&RepoPath> = match &reached {
            Some(r) => files.iter().copied().filter(|f| !r.contains(*f)).collect(),
            None => files,
        };
        directories(id, key, &rest, model, &mut diags);
    }

    if !unowned.is_empty() {
        let key = SystemKey {
            ecosystem: Ecosystem::Unowned,
            dir: RepoPath::root(),
        };
        let id = NodeId::system(key.clone());
        let node = Node {
            id: id.clone(),
            kind: NodeKind::Unowned,
            name: UNOWNED_NAME.to_owned(),
            source: RepoPath::root(),
            parent: None,
            confidence: Confidence::Unknown,
        };
        match model.add_node(node) {
            Ok(()) => {
                let files: BTreeSet<&RepoPath> = unowned.into_iter().collect();
                directories(&id, &key, &files, model, &mut diags);
            }
            Err(e) => diags.push(Diagnostic {
                path: RepoPath::root(),
                message: e.to_string(),
            }),
        }
    }
    diags.sort();
    diags
}

/// One `unknown` L2 directory node per top-level directory of `system` that
/// holds any of `files`. Files directly in the system's directory belong to
/// the system node itself.
fn directories(
    system: &NodeId,
    key: &SystemKey,
    files: &BTreeSet<&RepoPath>,
    model: &mut Model,
    diags: &mut Vec<Diagnostic>,
) {
    let depth = key.dir.segments().len();
    let tops: BTreeSet<&String> = files
        .iter()
        .filter(|f| f.segments().len() > depth + 1)
        .map(|f| &f.segments()[depth])
        .collect();
    for top in tops {
        let mut dir = key.dir.segments().to_vec();
        dir.push(top.clone());
        // The trailing `/` keeps directory ids apart from module ids.
        let node = Node {
            id: NodeId::within(Level::Module, key.clone(), vec![format!("{top}/")]),
            kind: NodeKind::Directory,
            name: format!("{top}/"),
            source: RepoPath::parse(&dir.join("/")).unwrap_or_else(|_| RepoPath::root()),
            parent: Some(system.clone()),
            confidence: Confidence::Unknown,
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: key.dir.clone(),
                message: e.to_string(),
            });
        }
    }
}
