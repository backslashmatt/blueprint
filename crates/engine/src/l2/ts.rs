//! L2 modules of an npm package (#138, DR 0012): each TypeScript or
//! JavaScript file is a module, as an ES module is, named by its path in the
//! package without the extension. An `index.*` file stands for its
//! directory, as `mod.rs` does in Rust; the package's own `index.*` is the
//! package root, which is the system itself at L2.
//!
//! When two files would be the same module (`x.ts` and `x.js`, or `x.ts`
//! and `x/index.ts`), the first in path order is the module and the others
//! are shown by directory. Files under `node_modules` are vendored code, not
//! the package's own, and are shown by directory too.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey, Visibility,
};

use crate::l1::Diagnostic;
use crate::lang;

/// The module each TypeScript or JavaScript file of the package at `key` is,
/// adding a node for each but the root. `None` if it has none.
pub fn analyse_package(
    system: &NodeId,
    key: &SystemKey,
    files: &BTreeSet<&RepoPath>,
    model: &mut Model,
    diags: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<RepoPath, Vec<String>>> {
    let depth = key.dir.segments().len();
    let mut taken: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut reached = BTreeMap::new();
    for file in files {
        // The package's own languages only: not a `.py` or `.cs` file in it.
        if lang::analyser_in(file, key.ecosystem).is_none() {
            continue;
        }
        let Some(module) = module_of(&file.segments()[depth..]) else {
            continue;
        };
        if !taken.insert(module.clone()) {
            continue;
        }
        if !module.is_empty() {
            let node = Node {
                id: NodeId::within(Level::Module, key.clone(), module.clone()),
                kind: NodeKind::Module,
                name: module.join("/"),
                source: (*file).clone(),
                parent: Some(system.clone()),
                confidence: Confidence::Exact,
                visibility: Some(Visibility::Public),
                code: None,
            };
            if let Err(e) = model.add_node(node) {
                diags.push(Diagnostic {
                    path: (*file).clone(),
                    message: e.to_string(),
                });
                continue;
            }
        }
        reached.insert((*file).clone(), module);
    }
    (!reached.is_empty()).then_some(reached)
}

/// The module a file at `rel` (its path in the package) is: the path
/// without the extension, and without a last `index`. `None` under
/// `node_modules`, or for a name with nothing before its extension.
fn module_of(rel: &[String]) -> Option<Vec<String>> {
    let (file, dirs) = rel.split_last()?;
    if dirs.iter().any(|d| d == "node_modules") {
        return None;
    }
    let (stem, _) = file.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    let mut module = dirs.to_vec();
    if stem != "index" {
        module.push(stem.to_owned());
    }
    Some(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(rel: &str) -> Option<Vec<String>> {
        let segs: Vec<String> = rel.split('/').map(str::to_owned).collect();
        module_of(&segs)
    }

    #[test]
    fn a_file_is_its_path_without_extension_and_index_is_its_directory() {
        let v = |s: &[&str]| Some(s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>());
        assert_eq!(m("src/util/x.ts"), v(&["src", "util", "x"]));
        assert_eq!(m("src/util/index.tsx"), v(&["src", "util"]));
        assert_eq!(m("index.js"), v(&[]));
        assert_eq!(m("types/a.d.ts"), v(&["types", "a.d"]));
        assert_eq!(m("node_modules/p/index.js"), None);
        assert_eq!(m("src/.js"), None);
        assert_eq!(m(".eslintrc.js"), v(&[".eslintrc"]));
    }
}
