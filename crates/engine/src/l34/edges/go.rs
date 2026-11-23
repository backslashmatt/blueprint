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
//! An import binds its alias, or the name its package's `package` clause
//! gives (none when the package's files disagree). An import from outside
//! the repository still claims the name the go tool would give it, so a
//! local package named alike is not bound in that file; neither is any
//! name two imports claim. Only exported names come through an import. An
//! import named `self` turns off `self.m()` resolution in its file.
//!
//! A cross-module import is as sure as the L1 dependency it goes through
//! (`inferred` for a `require` matched only by name).
//!
//! Known imprecision: a field of function type shadows a promoted method of
//! the same name, which the facts cannot see; such a `self.m()` call still
//! resolves to the method (calls are `inferred`).

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
    /// Each package's `package` clause name, when its files agree on one
    /// (an external `x_test` package aside): what an import binds.
    names: HashMap<(SystemKey, Vec<String>), Option<String>>,
    /// How sure each module's L1 dependency on another is: an import into
    /// it is no surer.
    dep_confidence: HashMap<(SystemKey, SystemKey), Confidence>,
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
                m.dep_confidence
                    .insert((from.clone(), to.clone()), e.confidence);
            }
        }
        for v in m.deps.values_mut() {
            v.sort();
            v.dedup();
        }
        for (_, place, module, facts) in parsed {
            if is_module(&place.system) {
                m.packages
                    .entry(place.system.clone())
                    .or_default()
                    .insert((*module).clone());
                let Some(name) = facts.package.as_ref().filter(|n| !n.ends_with("_test")) else {
                    continue;
                };
                let slot = m
                    .names
                    .entry((place.system.clone(), (*module).clone()))
                    .or_insert_with(|| Some(name.clone()));
                if slot.as_ref() != Some(name) {
                    *slot = None;
                }
            }
        }
        m
    }

    /// How sure an import from `from` into `to` is: `exact` within a
    /// module, else as sure as the L1 dependency.
    fn confidence(&self, from: &SystemKey, to: &SystemKey) -> Confidence {
        if from == to {
            Confidence::Exact
        } else {
            self.dep_confidence
                .get(&(from.clone(), to.clone()))
                .copied()
                .unwrap_or(Confidence::Inferred)
        }
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

    /// The name an import of a local package binds by default: its
    /// `package` clause's name. None when its files disagree or no file
    /// says: guessing from the directory can bind a name the file uses for
    /// something else.
    fn default_name(&self, system: &SystemKey, pkg: &[String]) -> Option<String> {
        self.names.get(&(system.clone(), pkg.to_vec()))?.clone()
    }
}

/// The name an import outside the repository binds by default, as the go
/// tool would guess it: the last path element without a `/vN` or `.vN`
/// version. Only used to see which names the file's other imports claim.
fn guessed_name(import: &str) -> String {
    let mut parts = import.rsplit('/');
    let mut last = parts.next().unwrap_or(import);
    let is_version =
        |s: &str| s.len() > 1 && s.starts_with('v') && s[1..].bytes().all(|b| b.is_ascii_digit());
    if is_version(last)
        && let Some(prev) = parts.next()
    {
        last = prev;
    }
    let last = match last.rsplit_once('.') {
        Some((base, v)) if is_version(v) => base,
        _ => last,
    };
    last.strip_prefix("go-").unwrap_or(last).to_owned()
}

/// What a file's imports bind.
struct Imports {
    /// name -> (system, package)
    named: HashMap<String, (SystemKey, Vec<String>)>,
    /// Dot imports: their exported names are the file's own.
    dot: Vec<(SystemKey, Vec<String>)>,
    /// An import named `self`: then `self.m()` may be that package, not
    /// the receiver.
    self_alias: bool,
}

impl Resolver<'_> {
    fn go_imports(&self, system: &SystemKey, facts: &FileFacts) -> Imports {
        let mut named: HashMap<String, (SystemKey, Vec<String>)> = HashMap::new();
        let mut doubtful: HashSet<String> = HashSet::new();
        let mut dot = Vec::new();
        let mut claimed: Vec<String> = Vec::new();
        let mut self_alias = false;
        for r in &facts.references {
            let Reference::Use { path, alias, .. } = r else {
                continue;
            };
            if alias.as_deref() == Some("self") {
                self_alias = true;
            }
            let [import] = path.as_slice() else { continue };
            let Some((s, pkg)) = self.go_modules.resolve(system, import) else {
                // An outside import still claims its name: a local package
                // named alike must not take calls meant for it.
                match alias.as_deref() {
                    Some("_" | ".") => {}
                    Some(a) => claimed.push(a.to_owned()),
                    None => claimed.push(guessed_name(import)),
                }
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
        for c in claimed {
            if named.contains_key(&c) {
                doubtful.insert(c);
            }
        }
        named.retain(|n, _| !doubtful.contains(n));
        Imports {
            named,
            dot,
            self_alias,
        }
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
                        let conf = self.go_modules.confidence(system, &s);
                        let key = EdgeKey {
                            from: from.clone(),
                            to: NodeId::within(Level::Module, s, pkg),
                            kind: EdgeKind::DependsOn,
                        };
                        found.push((key, conf));
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
            [pkg, name] if exported(name) => {
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
                    [s, _] if s == "self" && imports.self_alias => None,
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
                        // Only exported names come through a dot import;
                        // `min` stays the builtin.
                        let hits: Vec<NodeId> = imports
                            .dot
                            .iter()
                            .filter(|_| exported(f))
                            .filter_map(|(s, p)| exists(function(s, p, f)))
                            .collect();
                        match hits.as_slice() {
                            [one] => Some(one.clone()),
                            _ => None,
                        }
                    }),
                    [x, f] => match imports.named.get(x) {
                        Some((s, p)) => exported(f).then(|| exists(function(s, p, f))).flatten(),
                        // `T.M(t)`: a method expression on a type here.
                        None => exists(method(system, file_module, x, f)),
                    },
                    [pkg, t, m] => imports
                        .named
                        .get(pkg)
                        .filter(|_| exported(t) && exported(m))
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

/// Go's export rule: only a name starting with an upper-case letter is
/// visible outside its package.
fn exported(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
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
