//! Edges from Go references (#166, DR 0015).
//!
//! An import path resolves against the module path (`module` in `go.mod`,
//! the L1 node's name) of the importing module and of the local modules it
//! requires (its L1 edges): the longest module path that prefixes it, and
//! the package below it, if that package exists. The standard library and
//! external modules give no edge.
//!
//! - **depends-on (L2):** each import, from the file's package to the
//!   package it names, `exact`.
//! - **extends (L3):** each embedded type: `Base` in the package itself,
//!   `pkg.T` through an import.
//! - **calls (L4):** `F()` to a function of the package (or of a dot
//!   import, when exactly one has it), `pkg.F()` through an import, `T.M()`
//!   and `pkg.T.M()` (method expressions), and `self.m()` to a method of the
//!   receiver's type, or else of the one type it embeds (in this file) that
//!   has it. Calls through values need types and give none.
//!
//! An import binds its alias, or its package's directory name (for a
//! module's root package, the module path's last element without a `/vN`
//! suffix). A package whose `package` clause differs from its directory
//! name is then not reached through its clause name: a missing edge, never
//! a wrong one.

use std::collections::{HashMap, HashSet};

use blueprint_model::lang::{FileFacts, ItemKind, Reference};
use blueprint_model::{Confidence, Ecosystem, EdgeKey, EdgeKind, Level, Model, NodeId, SystemKey};

use super::super::{FUNCTIONS, Parsed};
use super::Resolver;

/// What Go resolution needs besides the model.
#[derive(Default)]
pub(crate) struct Modules {
    /// Each Go module's path, by system.
    paths: HashMap<SystemKey, String>,
    /// The local modules each module requires (its L1 depends-on edges).
    deps: HashMap<SystemKey, Vec<SystemKey>>,
    /// Each module's packages, as paths in the module (`[]` for the root).
    packages: HashMap<SystemKey, HashSet<Vec<String>>>,
}

/// Whether files of `system` are Go.
pub(super) fn is_module(system: &SystemKey) -> bool {
    system.ecosystem == Ecosystem::Go
}

impl Modules {
    pub(super) fn build(model: &Model, parsed: &[Parsed<'_>]) -> Modules {
        let mut m = Modules::default();
        for n in model.level(Level::System) {
            let key = n.id.system_key();
            // A `go.mod` with no `module` line is named by its directory:
            // that is no module path.
            if is_module(key) && n.confidence != Confidence::Unknown {
                m.paths.insert(key.clone(), n.name.clone());
            }
        }
        for e in model.edges() {
            let (from, to) = (e.key.from.system_key(), e.key.to.system_key());
            if e.key.kind == EdgeKind::DependsOn
                && e.key.from.level() == Level::System
                && is_module(from)
                && is_module(to)
                && from != to
            {
                m.deps.entry(from.clone()).or_default().push(to.clone());
            }
        }
        for v in m.deps.values_mut() {
            v.sort();
            v.dedup();
        }
        for (_, place, module, _) in parsed {
            if is_module(&place.system) {
                m.packages
                    .entry(place.system.clone())
                    .or_default()
                    .insert((*module).clone());
            }
        }
        m
    }

    /// The package an import path names, from `system`: in it or a module
    /// it requires, by the longest module path that prefixes the import.
    fn resolve(&self, system: &SystemKey, import: &str) -> Option<(SystemKey, Vec<String>)> {
        let mut best: Option<(usize, SystemKey, Vec<String>)> = None;
        for s in std::iter::once(system).chain(self.deps.get(system).into_iter().flatten()) {
            let Some(mp) = self.paths.get(s) else {
                continue;
            };
            let rest = if import == mp {
                Some("")
            } else {
                import
                    .strip_prefix(mp.as_str())
                    .and_then(|r| r.strip_prefix('/'))
            };
            let Some(rest) = rest else { continue };
            if best.as_ref().is_some_and(|(len, _, _)| *len >= mp.len()) {
                continue;
            }
            let pkg: Vec<String> = rest
                .split('/')
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect();
            best = Some((mp.len(), s.clone(), pkg));
        }
        let (_, s, pkg) = best?;
        self.packages
            .get(&s)
            .is_some_and(|p| p.contains(&pkg))
            .then_some((s, pkg))
    }

    /// The name an import binds by default: its package's directory name.
    fn default_name(&self, system: &SystemKey, pkg: &[String]) -> Option<String> {
        if let Some(last) = pkg.last() {
            return Some(last.clone());
        }
        let mp = self.paths.get(system)?;
        let mut parts = mp.rsplit('/');
        let last = parts.next()?;
        // `example.com/app/v2` is package `app`.
        let is_version = last.len() > 1
            && last.starts_with('v')
            && last[1..].bytes().all(|b| b.is_ascii_digit());
        let name = if is_version { parts.next()? } else { last };
        Some(name.to_owned())
    }
}

/// What a file's imports bind.
struct Imports {
    /// name -> (system, package)
    named: HashMap<String, (SystemKey, Vec<String>)>,
    /// Dot imports: their names are the file's own.
    dot: Vec<(SystemKey, Vec<String>)>,
}

impl Resolver<'_> {
    fn go_imports(&self, system: &SystemKey, facts: &FileFacts) -> Imports {
        let mut named: HashMap<String, (SystemKey, Vec<String>)> = HashMap::new();
        let mut doubtful: HashSet<String> = HashSet::new();
        let mut dot = Vec::new();
        for r in &facts.references {
            let Reference::Use { path, alias, .. } = r else {
                continue;
            };
            let [import] = path.as_slice() else { continue };
            let Some((s, pkg)) = self.go_modules.resolve(system, import) else {
                continue;
            };
            let name = match alias.as_deref() {
                Some("_") => continue,
                Some(".") => {
                    dot.push((s, pkg));
                    continue;
                }
                Some(a) => a.to_owned(),
                None => match self.go_modules.default_name(&s, &pkg) {
                    Some(n) => n,
                    None => continue,
                },
            };
            if named
                .get(&name)
                .is_some_and(|prev| *prev != (s.clone(), pkg.clone()))
            {
                doubtful.insert(name.clone());
            }
            named.insert(name, (s, pkg));
        }
        named.retain(|n, _| !doubtful.contains(n));
        Imports { named, dot }
    }

    /// One Go file's direct edges.
    pub(super) fn go_file_edges<'n>(
        &self,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<(EdgeKey, Confidence)> {
        let mut found: Vec<(EdgeKey, Confidence)> = self
            .go_file_calls(system, file_module, facts, node)
            .into_iter()
            .map(|k| (k, Confidence::Inferred))
            .collect();
        let from = NodeId::within(Level::Module, system.clone(), file_module.to_vec());
        let imports = self.go_imports(system, facts);
        for r in &facts.references {
            match r {
                Reference::Use { path, .. } => {
                    let [import] = path.as_slice() else { continue };
                    if let Some((s, pkg)) = self.go_modules.resolve(system, import) {
                        let key = EdgeKey {
                            from: from.clone(),
                            to: NodeId::within(Level::Module, s, pkg),
                            kind: EdgeKind::DependsOn,
                        };
                        found.push((key, Confidence::Exact));
                    }
                }
                Reference::Impl {
                    self_type, r#trait, ..
                } => {
                    let [ty] = self_type.as_slice() else { continue };
                    let ty_id = component(system, file_module, ty);
                    if self.model.node(&ty_id).is_none() {
                        continue;
                    }
                    if let Some(to) = self.go_type(system, file_module, &imports, r#trait)
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

    /// The L3 node of a type named `T` or `pkg.T` in a file.
    fn go_type(
        &self,
        system: &SystemKey,
        file_module: &[String],
        imports: &Imports,
        written: &[String],
    ) -> Option<NodeId> {
        let id = match written {
            [name] => component(system, file_module, name),
            [pkg, name] => {
                let (s, p) = imports.named.get(pkg)?;
                component(s, p, name)
            }
            _ => return None,
        };
        self.model.node(&id).is_some().then_some(id)
    }

    /// The `calls` edges of one Go file's functions.
    pub(super) fn go_file_calls<'n>(
        &self,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<EdgeKey> {
        let imports = self.go_imports(system, facts);
        // The types each type of this file embeds, for promoted methods.
        let mut embeds: HashMap<&str, Vec<NodeId>> = HashMap::new();
        for r in &facts.references {
            if let Reference::Impl {
                self_type, r#trait, ..
            } = r
                && let [ty] = self_type.as_slice()
                && let Some(b) = self.go_type(system, file_module, &imports, r#trait)
            {
                embeds.entry(ty.as_str()).or_default().push(b);
            }
        }
        let exists = |id: NodeId| self.model.node(&id).is_some().then_some(id);
        let mut out = Vec::new();
        for (i, item) in facts.items.iter().enumerate() {
            let Some(caller) = node(i) else { continue };
            let receiver = match &item.kind {
                ItemKind::Method { self_type, .. } => Some(self_type.as_str()),
                _ => None,
            };
            for call in &item.calls {
                let callee = match call.as_slice() {
                    [s, m] if s == "self" => receiver.and_then(|ty| {
                        exists(method(system, file_module, ty, m)).or_else(|| {
                            // One promoted method, from the one embedded
                            // type that has it.
                            let hits: Vec<NodeId> = embeds
                                .get(ty)
                                .into_iter()
                                .flatten()
                                .filter_map(|b| {
                                    exists(NodeId::within(
                                        Level::Code,
                                        b.system_key().clone(),
                                        [b.path(), std::slice::from_ref(m)].concat(),
                                    ))
                                })
                                .collect();
                            match hits.as_slice() {
                                [one] => Some(one.clone()),
                                _ => None,
                            }
                        })
                    }),
                    [f] => exists(function(system, file_module, f)).or_else(|| {
                        let hits: Vec<NodeId> = imports
                            .dot
                            .iter()
                            .filter_map(|(s, p)| exists(function(s, p, f)))
                            .collect();
                        match hits.as_slice() {
                            [one] => Some(one.clone()),
                            _ => None,
                        }
                    }),
                    [x, f] => match imports.named.get(x) {
                        Some((s, p)) => exists(function(s, p, f)),
                        // `T.M(t)`: a method expression on a type here.
                        None => exists(method(system, file_module, x, f)),
                    },
                    [pkg, t, m] => imports
                        .named
                        .get(pkg)
                        .and_then(|(s, p)| exists(method(s, p, t, m))),
                    _ => None,
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
}

fn component(system: &SystemKey, pkg: &[String], name: &str) -> NodeId {
    NodeId::within(
        Level::Component,
        system.clone(),
        [pkg, &[name.to_owned()]].concat(),
    )
}

fn function(system: &SystemKey, pkg: &[String], name: &str) -> NodeId {
    NodeId::within(
        Level::Code,
        system.clone(),
        [pkg, &[FUNCTIONS.to_owned(), name.to_owned()]].concat(),
    )
}

fn method(system: &SystemKey, pkg: &[String], ty: &str, name: &str) -> NodeId {
    NodeId::within(
        Level::Code,
        system.clone(),
        [pkg, &[ty.to_owned(), name.to_owned()]].concat(),
    )
}
