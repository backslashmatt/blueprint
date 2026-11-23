//! L2 analysis: modules inside each system (REQUIREMENTS §3.1: "a package,
//! namespace or directory with a public surface").
//!
//! - **Rust:** the module tree of each Cargo package, from `src/lib.rs` and
//!   `src/main.rs`, following `mod` declarations ([`rust`]).
//! - **TypeScript and JavaScript:** each file of an npm or Nx package is a
//!   module, named by its path ([`ts`]).
//! - **C#:** one module per namespace a .NET project's files declare
//!   ([`csharp`], DR 0013).
//! - **Python:** each file of a Python project is a module, named by its
//!   dotted path below the import root (`src/` or the project's directory)
//!   ([`python`], DR 0014).
//! - **No analyser:** files no analyser reached (a system with no Rust
//!   crate root, or a crate's tests, benches, build script and docs) are
//!   shown by top-level directory, with `unknown` confidence (§3.4).
//! - **Unowned:** files outside every system go under one synthetic
//!   `unowned` system, laid out by directory, so nothing disappears.
//!
//! Every L2 node's parent is its L1 system; nesting is in the node's path.

pub mod csharp;
mod lexer;
pub mod python;
pub mod rust;
pub mod ts;

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, Ecosystem, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey,
};

use crate::l1::{Diagnostic, Read};

/// The display name of the synthetic system for files outside every system.
pub const UNOWNED_NAME: &str = "(unowned)";

/// Where a file sits in the L1–L2 model, for the L3–L4 pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The system that owns it.
    pub system: SystemKey,
    /// The module it is, if its language's L2 pass reached it (a Rust
    /// module tree, or an npm package's file); otherwise it is shown by
    /// directory.
    pub module: Option<Vec<String>>,
}

/// What L2 reads from one Rust file: its `mod` items, and whether it is
/// clean UTF-8. Two versions of a file with the same shape give the same
/// L2 model.
#[derive(Debug, PartialEq)]
pub(crate) struct Shape(lexer::Scan, bool);

/// The [`Shape`] of a Rust file's bytes.
pub(crate) fn shape(bytes: &[u8]) -> Shape {
    let text = String::from_utf8_lossy(bytes);
    Shape(lexer::scan(&text), std::str::from_utf8(bytes).is_ok())
}

/// Adds L2 nodes for every L1 system already in `model`, and the unowned
/// system for files outside all of them. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    run(paths, read, model, None)
}

/// [`analyse`], also returning where each file was placed, for L3–L4.
pub fn analyse_placed(
    paths: &[RepoPath],
    read: Read<'_>,
    model: &mut Model,
) -> (Vec<Diagnostic>, BTreeMap<RepoPath, Placement>) {
    let mut placed = BTreeMap::new();
    let diags = run(paths, read, model, Some(&mut placed));
    (diags, placed)
}

/// The L2 pass. Placements are recorded only when asked for: on large
/// repositories building them costs more than the 5% benchmark gate.
fn run(
    paths: &[RepoPath],
    read: Read<'_>,
    model: &mut Model,
    mut placed: Option<&mut BTreeMap<RepoPath, Placement>>,
) -> Vec<Diagnostic> {
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
        let reached = match key.ecosystem {
            Ecosystem::Cargo => {
                rust::analyse_crate(id, key, &files, read, model, &mut diags, placed.is_some())
            }
            Ecosystem::Npm | Ecosystem::Nx => {
                ts::analyse_package(id, key, &files, model, &mut diags)
            }
            Ecosystem::Dotnet => csharp::analyse_project(id, key, &files, read, model, &mut diags),
            Ecosystem::Python => python::analyse_project(id, key, &files, model, &mut diags),
            _ => None,
        };
        // Whatever the module tree did not reach (tests, benches, examples,
        // build scripts, docs, files behind unparsed macros) still shows up,
        // by directory.
        let rest: BTreeSet<&RepoPath> = match &reached {
            Some(r) => files
                .iter()
                .copied()
                .filter(|f| !r.contains_key(*f))
                .collect(),
            None => files,
        };
        if let Some(placed) = placed.as_deref_mut() {
            for (path, module) in reached.into_iter().flatten() {
                placed.insert(
                    path,
                    Placement {
                        system: key.clone(),
                        module: Some(module),
                    },
                );
            }
            for f in &rest {
                placed.insert(
                    (*f).clone(),
                    Placement {
                        system: key.clone(),
                        module: None,
                    },
                );
            }
        }
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
            visibility: None,
            code: None,
        };
        match model.add_node(node) {
            Ok(()) => {
                let files: BTreeSet<&RepoPath> = unowned.into_iter().collect();
                directories(&id, &key, &files, model, &mut diags);
                if let Some(placed) = placed {
                    for f in files {
                        placed.insert(
                            f.clone(),
                            Placement {
                                system: key.clone(),
                                module: None,
                            },
                        );
                    }
                }
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
/// the system node itself; L3 gives them a `./` directory when it needs one.
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
            visibility: None,
            code: None,
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: key.dir.clone(),
                message: e.to_string(),
            });
        }
    }
}
