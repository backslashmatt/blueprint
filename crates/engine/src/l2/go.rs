//! L2 modules of a Go module (#164, DR 0015): each directory holding `.go`
//! files is a package, and the package is the module, named by its path
//! below the module's directory (`internal/store`). The module root's
//! package is the system itself. A package's files share it: `_test.go`
//! files, an external `x_test` package, and every file's items, which the
//! L3–L4 pass places by the file's package.
//!
//! Directories the go tool ignores (`testdata`, `vendor`, a name starting
//! with `_` or `.`) are shown by directory, as L1 ignores them too.
//! A package under an `internal` directory is restricted: only its parent's
//! tree may import it. Any other is public.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey, Visibility,
};

use crate::l1::Diagnostic;
use crate::l1::go::ignored;
use crate::l1::path::repo_path;
use crate::lang;

/// The package each Go file of the module at `key` is in, adding a node for
/// each package but the root. `None` if it has no Go files.
pub fn analyse_module(
    system: &NodeId,
    key: &SystemKey,
    files: &BTreeSet<&RepoPath>,
    model: &mut Model,
    diags: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<RepoPath, Vec<String>>> {
    let depth = key.dir.segments().len();
    let mut added: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut reached = BTreeMap::new();
    for file in files {
        if lang::analyser_in(file, key.ecosystem).is_none() || ignored(file) {
            continue;
        }
        let segs = file.segments();
        let module: Vec<String> = segs[depth..segs.len() - 1].to_vec();
        if !module.is_empty() && !added.contains(&module) {
            let node = Node {
                id: NodeId::within(Level::Module, key.clone(), module.clone()),
                kind: NodeKind::Module,
                name: module.join("/"),
                source: repo_path(&segs[..segs.len() - 1]),
                parent: Some(system.clone()),
                confidence: Confidence::Exact,
                visibility: Some(if module.iter().any(|s| s == "internal") {
                    Visibility::Restricted
                } else {
                    Visibility::Public
                }),
                code: None,
            };
            if let Err(e) = model.add_node(node) {
                diags.push(Diagnostic {
                    path: (*file).clone(),
                    message: e.to_string(),
                });
                continue;
            }
            added.insert(module.clone());
        }
        reached.insert((*file).clone(), module);
    }
    (!reached.is_empty()).then_some(reached)
}
