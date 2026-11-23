//! Edges from Python references (#157, DR 0014).
//!
//! A module path resolves in the importing project's own modules, then in
//! those of the Python projects it depends on (its L1 edges), as the
//! longest prefix that is a module; what follows is an item of it. The
//! standard library and third-party packages give no edge.
//!
//! - **depends-on (L2):** each `import`. An absolute one (`import a.b`,
//!   `from a.b import c`) as above; a relative one (`from . import x`,
//!   `from ..p import y`) from the importing module's package, one package
//!   up per dot after the first, in its own project only. `exact` when what
//!   follows the module is nothing, `*`, or an item of it; `inferred` when
//!   it names something the module does not define (a re-export, a
//!   variable).
//! - **extends (L3):** each base class, by its name in the file: a class of
//!   the file's own module, else one the file imports (`from m import B`,
//!   or `m.B` through `import m`).
//! - **calls (L4):** the analyser's call paths through the same names: `f()`
//!   to a function of the module or one it imports, `m.f()`, `C()` to
//!   `C.__init__`, `C.m()`, and `self.m()` to a method of the caller's class
//!   or, failing that, of a base class this file resolves. Calls through
//!   other values need types and give none.

use std::collections::{HashMap, HashSet};

use blueprint_model::lang::{FileFacts, ItemKind, Reference};
use blueprint_model::{
    Confidence, Ecosystem, EdgeKey, EdgeKind, Level, Model, NodeId, RepoPath, SystemKey,
};

use super::super::{FUNCTIONS, Parsed};
use super::Resolver;

/// What Python resolution needs besides the model and the index.
#[derive(Default)]
pub(crate) struct Projects {
    /// The Python projects each depends on (its L1 depends-on edges).
    deps: HashMap<SystemKey, Vec<SystemKey>>,
    /// Each project's modules, the root (`[]`) included when a file is it.
    modules: HashMap<SystemKey, HashSet<Vec<String>>>,
}

/// Whether files of `system` are Python.
pub(super) fn is_project(system: &SystemKey) -> bool {
    system.ecosystem == Ecosystem::Python
}

impl Projects {
    pub(super) fn build(model: &Model, parsed: &[Parsed<'_>]) -> Projects {
        let mut deps: HashMap<SystemKey, Vec<SystemKey>> = HashMap::new();
        for e in model.edges() {
            let (from, to) = (e.key.from.system_key(), e.key.to.system_key());
            if e.key.kind == EdgeKind::DependsOn
                && e.key.from.level() == Level::System
                && is_project(from)
                && is_project(to)
                && from != to
            {
                deps.entry(from.clone()).or_default().push(to.clone());
            }
        }
        for v in deps.values_mut() {
            v.sort();
            v.dedup();
        }
        let mut modules: HashMap<SystemKey, HashSet<Vec<String>>> = HashMap::new();
        for (_, place, module, _) in parsed {
            if is_project(&place.system) {
                modules
                    .entry(place.system.clone())
                    .or_default()
                    .insert((*module).clone());
            }
        }
        Projects { deps, modules }
    }

    fn is_module(&self, system: &SystemKey, module: &[String]) -> bool {
        self.modules.get(system).is_some_and(|m| m.contains(module))
    }

    /// The longest prefix of `path` that is a module of `system`, at least
    /// `min` long, and what follows it.
    fn split<'p>(
        &self,
        system: &SystemKey,
        path: &'p [String],
        min: usize,
    ) -> Option<(Vec<String>, &'p [String])> {
        (min..=path.len())
            .rev()
            .find(|&n| self.is_module(system, &path[..n]))
            .map(|n| (path[..n].to_vec(), &path[n..]))
    }

    /// An absolute module path from `system`: its own modules first, then
    /// those of the projects it depends on. A path of none of them is
    /// external. The empty prefix (a project's root `__init__`) never
    /// matches an absolute import: `import x` names a top-level module.
    fn absolute<'p>(
        &self,
        system: &SystemKey,
        path: &'p [String],
    ) -> Option<(SystemKey, Vec<String>, &'p [String])> {
        std::iter::once(system)
            .chain(self.deps.get(system).into_iter().flatten())
            .find_map(|s| self.split(s, path, 1).map(|(m, rest)| (s.clone(), m, rest)))
    }
}

/// Where a module's relative imports start: its package. An `__init__`
/// file is its package; any other module's package is its parent.
fn package_of(path: &RepoPath, module: &[String]) -> Vec<String> {
    let init = path
        .segments()
        .last()
        .is_some_and(|f| f.starts_with("__init__."));
    if init {
        module.to_vec()
    } else {
        module
            .split_last()
            .map(|(_, p)| p.to_vec())
            .unwrap_or_default()
    }
}

/// A reference's path with a relative prefix (`.`, `..`) made absolute
/// within the project, and how many leading segments are fixed (a relative
/// import may only name modules at or below its base). `None` when it
/// climbs above the import root.
fn absolute_path(
    path: &RepoPath,
    module: &[String],
    written: &[String],
) -> Option<(Vec<String>, usize, bool)> {
    let first = written.first()?;
    if !first.is_empty() && first.chars().all(|c| c == '.') {
        let mut base = package_of(path, module);
        for _ in 1..first.len() {
            base.pop()?;
        }
        let fixed = base.len();
        base.extend(written[1..].iter().cloned());
        Some((base, fixed, true))
    } else {
        Some((written.to_vec(), 1, false))
    }
}

/// The names a file binds through its imports: name -> the path written.
///
/// The analyser writes `import a.b` and `from a import b` alike, as
/// `["a", "b"]`: the first binds `a`, the second `b`. Both are bound. A
/// name bound only by the reading that was not meant is one the code
/// cannot use without a `NameError`, so it resolves nothing real.
fn py_names(facts: &FileFacts) -> HashMap<&str, Vec<String>> {
    let mut bound: HashMap<&str, Vec<String>> = HashMap::new();
    for r in &facts.references {
        let Reference::Use {
            path: written,
            alias,
            ..
        } = r
        else {
            continue;
        };
        let Some((last, _)) = written.split_last() else {
            continue;
        };
        if last == "*" {
            continue;
        }
        if let Some(a) = alias {
            bound.insert(a.as_str(), written.clone());
            continue;
        }
        if !last.starts_with('.') {
            bound.insert(last.as_str(), written.clone());
        }
        if let Some(first) = written.first().filter(|f| !f.starts_with('.')) {
            bound
                .entry(first.as_str())
                .or_insert_with(|| vec![first.clone()]);
        }
    }
    bound
}

impl Resolver<'_> {
    /// Resolves a written import or name path from `system`'s module
    /// `module` (file `path`): the target system, module, and the rest.
    fn py_resolve(
        &self,
        path: &RepoPath,
        system: &SystemKey,
        module: &[String],
        written: &[String],
    ) -> Option<(SystemKey, Vec<String>, Vec<String>)> {
        let (full, fixed, relative) = absolute_path(path, module, written)?;
        let mut full = full;
        if full.last().is_some_and(|s| s == "*") {
            full.pop();
        }
        if relative {
            let (m, rest) = self.py_projects.split(system, &full, fixed)?;
            Some((system.clone(), m, rest.to_vec()))
        } else {
            let (s, m, rest) = self.py_projects.absolute(system, &full)?;
            Some((s, m, rest.to_vec()))
        }
    }

    /// What a dotted name used in `module` names: a system, a module and
    /// the item path after it (`["K"]`, `["K", "m"]`, `["f"]`).
    fn py_lookup(
        &self,
        path: &RepoPath,
        system: &SystemKey,
        module: &[String],
        names: &HashMap<&str, Vec<String>>,
        written: &[String],
    ) -> Option<(SystemKey, Vec<String>, Vec<String>)> {
        let (first, rest) = written.split_first()?;
        // The module's own class or function.
        if self.py_defines(system, module, first) {
            return Some((system.clone(), module.to_vec(), written.to_vec()));
        }
        let target = names.get(first.as_str())?;
        let full = [target.as_slice(), rest].concat();
        self.py_resolve(path, system, module, &full)
    }

    /// Whether `module` of `system` defines a class or function `name`.
    fn py_defines(&self, system: &SystemKey, module: &[String], name: &str) -> bool {
        self.has_item(system, module, name)
            || self
                .model
                .node(&function_id(system, module, name))
                .is_some()
    }

    /// One Python file's direct edges.
    pub(super) fn py_file_edges<'n>(
        &self,
        path: &RepoPath,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<(EdgeKey, Confidence)> {
        let mut found: Vec<(EdgeKey, Confidence)> = self
            .py_file_calls(path, system, file_module, facts, node)
            .into_iter()
            .map(|k| (k, Confidence::Inferred))
            .collect();
        let from = NodeId::within(Level::Module, system.clone(), file_module.to_vec());
        let names = py_names(facts);
        for r in &facts.references {
            match r {
                Reference::Use { path: written, .. } => {
                    let Some((sys, m, rest)) = self.py_resolve(path, system, file_module, written)
                    else {
                        continue;
                    };
                    let exact = match rest.as_slice() {
                        [] => true,
                        [name, ..] => self.py_defines(&sys, &m, name),
                    };
                    let key = EdgeKey {
                        from: from.clone(),
                        to: NodeId::within(Level::Module, sys, m),
                        kind: EdgeKind::DependsOn,
                    };
                    let conf = if exact {
                        Confidence::Exact
                    } else {
                        Confidence::Inferred
                    };
                    found.push((key, conf));
                }
                Reference::Impl {
                    self_type, r#trait, ..
                } => {
                    let [ty] = self_type.as_slice() else { continue };
                    let ty_id = NodeId::within(
                        Level::Component,
                        system.clone(),
                        [file_module, std::slice::from_ref(ty)].concat(),
                    );
                    if self.model.node(&ty_id).is_none() {
                        continue;
                    }
                    if let Some(to) = self.py_class(path, system, file_module, &names, r#trait)
                        && to != ty_id
                    {
                        found.push((
                            EdgeKey {
                                from: ty_id,
                                to,
                                kind: EdgeKind::Extends,
                            },
                            Confidence::Exact,
                        ));
                    }
                }
            }
        }
        found
    }

    /// The L3 node of the class a written name names, if it is one.
    fn py_class(
        &self,
        path: &RepoPath,
        system: &SystemKey,
        module: &[String],
        names: &HashMap<&str, Vec<String>>,
        written: &[String],
    ) -> Option<NodeId> {
        let (s, m, rest) = self.py_lookup(path, system, module, names, written)?;
        if rest.is_empty() {
            return None;
        }
        let id = NodeId::within(Level::Component, s, [m, vec![rest.join(".")]].concat());
        self.model.node(&id).is_some().then_some(id)
    }

    /// The `calls` edges of one Python file's functions.
    pub(super) fn py_file_calls<'n>(
        &self,
        path: &RepoPath,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<EdgeKey> {
        let names = py_names(facts);
        // Each class's bases this file resolves, for `self.m()`.
        let mut bases: HashMap<&str, Vec<NodeId>> = HashMap::new();
        for r in &facts.references {
            if let Reference::Impl {
                self_type, r#trait, ..
            } = r
                && let [ty] = self_type.as_slice()
                && let Some(b) = self.py_class(path, system, file_module, &names, r#trait)
            {
                bases.entry(ty.as_str()).or_default().push(b);
            }
        }
        let mut out = Vec::new();
        for (i, item) in facts.items.iter().enumerate() {
            let Some(caller) = node(i) else { continue };
            let own_class = match &item.kind {
                ItemKind::Method { self_type, .. } => Some(self_type.as_str()),
                _ => None,
            };
            for call in &item.calls {
                let callee = match call.as_slice() {
                    [s, rest @ ..] if s == "self" => own_class.and_then(|class| {
                        let [method] = rest else { return None };
                        let own = NodeId::within(
                            Level::Code,
                            system.clone(),
                            [file_module, &[class.to_owned(), method.clone()]].concat(),
                        );
                        if self.model.node(&own).is_some() {
                            return Some(own);
                        }
                        bases.get(class).into_iter().flatten().find_map(|b| {
                            let id = NodeId::within(
                                Level::Code,
                                b.system_key().clone(),
                                [b.path(), std::slice::from_ref(method)].concat(),
                            );
                            self.model.node(&id).is_some().then_some(id)
                        })
                    }),
                    written => self
                        .py_lookup(path, system, file_module, &names, written)
                        .and_then(|(s, m, rest)| self.py_callee(&s, &m, &rest)),
                };
                if let Some(callee) = callee
                    && callee != *caller
                {
                    out.push(EdgeKey {
                        from: caller.clone(),
                        to: callee,
                        kind: EdgeKind::Calls,
                    });
                }
            }
        }
        out
    }

    /// The L4 node a call to `rest` in module `m` reaches: a function
    /// (`f`), a class's constructor (`C` is `C.__init__`), or a method
    /// (`C.m`, `Outer.Inner.m`).
    fn py_callee(&self, system: &SystemKey, m: &[String], rest: &[String]) -> Option<NodeId> {
        let candidates: Vec<NodeId> = match rest {
            [] => Vec::new(),
            [name] => vec![
                function_id(system, m, name),
                NodeId::within(
                    Level::Code,
                    system.clone(),
                    [m, &[name.clone(), "__init__".to_owned()]].concat(),
                ),
            ],
            [class @ .., method] => vec![NodeId::within(
                Level::Code,
                system.clone(),
                [m, &[class.join("."), method.clone()]].concat(),
            )],
        };
        candidates
            .into_iter()
            .find(|id| self.model.node(id).is_some())
    }
}

fn function_id(system: &SystemKey, module: &[String], name: &str) -> NodeId {
    NodeId::within(
        Level::Code,
        system.clone(),
        [module, &[FUNCTIONS.to_owned(), name.to_owned()]].concat(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> RepoPath {
        RepoPath::parse(s).unwrap_or_else(|_| unreachable!())
    }

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn relative_imports_climb_from_the_package() {
        // `pkg/sub/mod.py` is module pkg.sub.mod, package pkg.sub.
        let at = p("src/pkg/sub/mod.py");
        let m = v(&["pkg", "sub", "mod"]);
        assert_eq!(
            absolute_path(&at, &m, &v(&[".", "x"])),
            Some((v(&["pkg", "sub", "x"]), 2, true))
        );
        assert_eq!(
            absolute_path(&at, &m, &v(&["..", "y", "z"])),
            Some((v(&["pkg", "y", "z"]), 1, true))
        );
        assert_eq!(absolute_path(&at, &m, &v(&["....", "x"])), None);
        // An `__init__` is its own package.
        let init = p("src/pkg/__init__.py");
        assert_eq!(
            absolute_path(&init, &v(&["pkg"]), &v(&[".", "a"])),
            Some((v(&["pkg", "a"]), 1, true))
        );
        assert_eq!(
            absolute_path(&at, &m, &v(&["os", "path"])),
            Some((v(&["os", "path"]), 1, false))
        );
    }
}
