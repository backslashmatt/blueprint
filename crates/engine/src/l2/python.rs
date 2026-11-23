//! L2 modules of a Python project (#154, DR 0014): each `.py` or `.pyi`
//! file is a module, as Python imports it, named by its path below the
//! import root without the extension, dotted (`pkg.sub.mod`). A package's
//! `__init__` is the package (`pkg/__init__.py` is `pkg`).
//!
//! The import root is the project's `src/` directory for files under it
//! (the src layout), and the project's directory for every other file
//! (the flat layout, tests and scripts). A file directly at an import
//! root named `__init__` is the system itself.
//!
//! When two files would be the same module (`x.py` and `x.pyi`, or
//! `src/pkg/` and `pkg/`), the first in path order is the module and the
//! others are shown by directory. Files under a virtual environment, cache
//! or other ignored directory (as L1 ignores them) are shown by directory.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey, Visibility,
};

use crate::l1::Diagnostic;
use crate::l1::python::ignored_dir;
use crate::lang;

/// The module each Python file of the project at `key` is, adding a node
/// for each but the root. `None` if it has none.
pub fn analyse_project(
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
                name: module.join("."),
                source: (*file).clone(),
                parent: Some(system.clone()),
                confidence: Confidence::Exact,
                // `_x` is private; a dunder (`__main__`) is not.
                visibility: Some(if module.iter().any(|s| is_private(s)) {
                    Visibility::Private
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
        }
        reached.insert((*file).clone(), module);
    }
    (!reached.is_empty()).then_some(reached)
}

/// Python's naming convention for one name.
fn is_private(name: &str) -> bool {
    let dunder = name.len() > 4 && name.starts_with("__") && name.ends_with("__");
    name.starts_with('_') && !dunder
}

/// The module a file at `rel` (its path in the project) is. `None` under an
/// ignored directory, or for a name with nothing before its extension.
pub(crate) fn module_of(rel: &[String]) -> Option<Vec<String>> {
    let (file, dirs) = rel.split_last()?;
    if dirs.iter().any(|d| ignored_dir(d)) {
        return None;
    }
    let stem = file
        .strip_suffix(".py")
        .or_else(|| file.strip_suffix(".pyi"))?;
    if stem.is_empty() {
        return None;
    }
    // The src layout: `src/` is the import root, not a package.
    let dirs = match dirs.split_first() {
        Some((first, rest)) if first == "src" => rest,
        _ => dirs,
    };
    let mut module = dirs.to_vec();
    if stem != "__init__" {
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
    fn a_file_is_its_dotted_path_and_init_its_package() {
        let v = |s: &[&str]| Some(s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>());
        assert_eq!(m("src/pkg/sub/mod.py"), v(&["pkg", "sub", "mod"]));
        assert_eq!(m("src/pkg/__init__.py"), v(&["pkg"]));
        assert_eq!(m("pkg/types.pyi"), v(&["pkg", "types"]));
        assert_eq!(m("tests/test_a.py"), v(&["tests", "test_a"]));
        assert_eq!(m("__init__.py"), v(&[]));
        assert_eq!(m("src/__init__.py"), v(&[]));
        assert_eq!(m(".venv/lib/x.py"), None);
        assert_eq!(m("pkg/__pycache__/x.py"), None);
        assert_eq!(m("pkg/.py"), None);
        assert_eq!(m("pkg/x.pyc"), None);
    }
}
