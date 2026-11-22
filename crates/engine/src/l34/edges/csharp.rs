//! Edges from C# references (#150, DR 0013).
//!
//! A name resolves in the project's own namespaces and those of the
//! projects it references (its L1 edges), as the compiler sees them;
//! external namespaces (`System.*`, packages) give no edge.
//!
//! - **depends-on (L2):** `using A.B;` (and `using static A.T;`, `using X =
//!   A.B;`) from each namespace of the file's items that the `using` is in
//!   scope for, to the namespace it names, `exact`.
//! - **extends / implements (L3):** each base of a type, found in the
//!   type's namespace and its enclosing ones, then the file's `using`s, or
//!   as a qualified name; `implements` when it is an interface, `extends`
//!   otherwise. A base that does not resolve gives no edge.
//! - **calls (L4):** `T.M()` and `A.B.T.M()` resolved as types are; `M()` in
//!   the caller's own type. Calls through instances need types and give
//!   none.

use std::collections::{HashMap, HashSet};

use blueprint_model::lang::{FileFacts, ItemKind, Reference};
use blueprint_model::{Confidence, Ecosystem, EdgeKey, EdgeKind, Level, Model, NodeId, SystemKey};

use super::super::Parsed;
use super::Resolver;

/// What C# resolution needs besides the model and the index.
#[derive(Default)]
pub(crate) struct Projects {
    /// The projects each project references (its L1 depends-on edges).
    refs: HashMap<SystemKey, Vec<SystemKey>>,
    /// Interfaces, by system, namespace and name: a base that is one is
    /// implemented, any other extended.
    interfaces: HashSet<(SystemKey, Vec<String>, String)>,
}

/// Whether files of `system` are C#.
pub(super) fn is_project(system: &SystemKey) -> bool {
    system.ecosystem == Ecosystem::Dotnet
}

impl Projects {
    pub(super) fn build(model: &Model, parsed: &[Parsed<'_>]) -> Projects {
        let mut refs: HashMap<SystemKey, Vec<SystemKey>> = HashMap::new();
        for e in model.edges() {
            let (from, to) = (e.key.from.system_key(), e.key.to.system_key());
            if e.key.kind == EdgeKind::DependsOn
                && e.key.from.level() == Level::System
                && is_project(from)
                && is_project(to)
                && from != to
            {
                refs.entry(from.clone()).or_default().push(to.clone());
            }
        }
        for v in refs.values_mut() {
            v.sort();
            v.dedup();
        }
        let mut p = Projects {
            refs,
            interfaces: HashSet::new(),
        };
        for (_, place, module, facts) in parsed {
            if is_project(&place.system) {
                p.rekind(&place.system, module, None, Some(facts));
            }
        }
        p
    }

    /// Replaces a file's interfaces: `old` facts out, `new` in.
    pub(crate) fn rekind(
        &mut self,
        system: &SystemKey,
        file_module: &[String],
        old: Option<&FileFacts>,
        new: Option<&FileFacts>,
    ) {
        let keys = |f: &FileFacts| -> Vec<(SystemKey, Vec<String>, String)> {
            f.items
                .iter()
                .filter(|i| {
                    matches!(&i.kind, ItemKind::Type { keyword } if keyword.ends_with("interface"))
                })
                .map(|i| {
                    (
                        system.clone(),
                        [file_module, i.module.as_slice()].concat(),
                        i.name.clone(),
                    )
                })
                .collect()
        };
        for k in old.map(keys).unwrap_or_default() {
            self.interfaces.remove(&k);
        }
        for k in new.map(keys).unwrap_or_default() {
            self.interfaces.insert(k);
        }
    }

    /// `system` and the projects it references, where its names resolve.
    fn visible<'a>(&'a self, system: &'a SystemKey) -> impl Iterator<Item = &'a SystemKey> {
        std::iter::once(system).chain(self.refs.get(system).into_iter().flatten())
    }
}

/// A file's `using`s in scope at namespace `at`: the namespaces brought in
/// whole, and aliases (name -> path).
struct Usings<'f> {
    globs: Vec<(&'f [String], &'f [String])>,
    aliases: Vec<(&'f [String], &'f str, &'f [String])>,
}

impl<'f> Usings<'f> {
    fn of(facts: &'f FileFacts) -> Usings<'f> {
        let mut u = Usings {
            globs: Vec::new(),
            aliases: Vec::new(),
        };
        for r in &facts.references {
            if let Reference::Use {
                path,
                alias,
                module,
            } = r
            {
                match (alias, path.split_last()) {
                    (Some(a), _) => u.aliases.push((module, a.as_str(), path)),
                    (None, Some((star, base))) if star == "*" => u.globs.push((module, base)),
                    _ => {}
                }
            }
        }
        u
    }

    fn globs_at(&self, at: &[String]) -> impl Iterator<Item = &'f [String]> + '_ {
        let at = at.to_vec();
        self.globs
            .iter()
            .filter(move |(m, _)| at.starts_with(m))
            .map(|(_, b)| *b)
    }

    fn alias_at(&self, at: &[String], name: &str) -> Option<&'f [String]> {
        self.aliases
            .iter()
            .find(|(m, a, _)| *a == name && at.starts_with(m))
            .map(|(_, _, p)| *p)
    }
}

impl Resolver<'_> {
    fn is_namespace(&self, system: &SystemKey, ns: &[String]) -> bool {
        ns.is_empty()
            || self
                .index
                .modules
                .get(system)
                .is_some_and(|m| m.contains(ns))
    }

    /// The type a (possibly qualified) name used in namespace `at` names:
    /// its system, namespace and name.
    fn cs_type(
        &self,
        system: &SystemKey,
        at: &[String],
        path: &[String],
        usings: &Usings<'_>,
    ) -> Option<(SystemKey, Vec<String>, String)> {
        let (first, rest) = path.split_first()?;
        // An alias stands for its path.
        if let Some(target) = usings.alias_at(at, first) {
            let whole = [target, rest].concat();
            return self.qualified(system, &whole);
        }
        if rest.is_empty() {
            // The namespace and its enclosing ones, then the usings.
            let mut scopes: Vec<Vec<String>> =
                (0..=at.len()).rev().map(|n| at[..n].to_vec()).collect();
            scopes.extend(usings.globs_at(at).map(<[String]>::to_vec));
            for ns in scopes {
                for sys in self.projects.visible(system) {
                    if self.has_item(sys, &ns, first) {
                        return Some((sys.clone(), ns, first.clone()));
                    }
                }
            }
            return None;
        }
        // `Outer.Inner` from a using's namespace, or a qualified name.
        let nested = path.join(".");
        for ns in usings
            .globs_at(at)
            .chain((0..=at.len()).rev().map(|n| &at[..n]))
        {
            for sys in self.projects.visible(system) {
                if self.has_item(sys, ns, &nested) {
                    return Some((sys.clone(), ns.to_vec(), nested.clone()));
                }
            }
        }
        self.qualified(system, path)
    }

    /// `A.B.T` (or `A.B.Outer.Inner`) as a namespace and a type in it.
    fn qualified(
        &self,
        system: &SystemKey,
        path: &[String],
    ) -> Option<(SystemKey, Vec<String>, String)> {
        for split in (0..path.len()).rev() {
            let (ns, name) = (&path[..split], path[split..].join("."));
            for sys in self.projects.visible(system) {
                if self.is_namespace(sys, ns) && self.has_item(sys, ns, &name) {
                    return Some((sys.clone(), ns.to_vec(), name));
                }
            }
        }
        None
    }

    /// One C# file's direct edges.
    pub(super) fn cs_file_edges<'n>(
        &self,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<(EdgeKey, Confidence)> {
        let mut found: Vec<(EdgeKey, Confidence)> = self
            .cs_file_calls(system, file_module, facts, node)
            .into_iter()
            .map(|k| (k, Confidence::Inferred))
            .collect();
        let usings = Usings::of(facts);
        // The namespaces the file's items are in.
        let mut namespaces: Vec<Vec<String>> = facts
            .items
            .iter()
            .map(|i| [file_module, i.module.as_slice()].concat())
            .collect();
        namespaces.sort();
        namespaces.dedup();
        for r in &facts.references {
            match r {
                Reference::Use {
                    path,
                    alias,
                    module,
                } => {
                    let base: &[String] = match (alias, path.split_last()) {
                        (None, Some((star, base))) if star == "*" => base,
                        _ => path,
                    };
                    // A namespace, or a type (`using static`, an alias).
                    let target = self
                        .projects
                        .visible(system)
                        .find(|s| !base.is_empty() && self.is_namespace(s, base))
                        .map(|s| (s.clone(), base.to_vec()))
                        .or_else(|| self.qualified(system, base).map(|(s, ns, _)| (s, ns)));
                    let Some((sys, ns)) = target else { continue };
                    for from in namespaces.iter().filter(|n| n.starts_with(module)) {
                        let key = EdgeKey {
                            from: NodeId::within(Level::Module, system.clone(), from.clone()),
                            to: NodeId::within(Level::Module, sys.clone(), ns.clone()),
                            kind: EdgeKind::DependsOn,
                        };
                        found.push((key, Confidence::Exact));
                    }
                }
                Reference::Impl {
                    self_type,
                    r#trait,
                    module,
                    ..
                } => {
                    let [ty] = self_type.as_slice() else { continue };
                    let at = [file_module, module.as_slice()].concat();
                    let from = NodeId::within(
                        Level::Component,
                        system.clone(),
                        [at.clone(), vec![ty.clone()]].concat(),
                    );
                    if self.model.node(&from).is_none() {
                        continue;
                    }
                    let Some((sys, ns, name)) = self.cs_type(system, &at, r#trait, &usings) else {
                        continue;
                    };
                    let interface =
                        self.projects
                            .interfaces
                            .contains(&(sys.clone(), ns.clone(), name.clone()));
                    let to = NodeId::within(Level::Component, sys, [ns, vec![name]].concat());
                    if self.model.node(&to).is_some() {
                        let kind = if interface {
                            EdgeKind::Implements
                        } else {
                            EdgeKind::Extends
                        };
                        found.push((EdgeKey { from, to, kind }, Confidence::Exact));
                    }
                }
            }
        }
        found
    }

    /// The `calls` edges of one C# file's members.
    pub(super) fn cs_file_calls<'n>(
        &self,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<EdgeKey> {
        let usings = Usings::of(facts);
        let mut out = Vec::new();
        for (i, item) in facts.items.iter().enumerate() {
            let (Some(caller), ItemKind::Method { .. }) = (node(i), &item.kind) else {
                continue;
            };
            let at = [file_module, item.module.as_slice()].concat();
            for call in &item.calls {
                let Some((name, owner)) = call.split_last() else {
                    continue;
                };
                let callee = if owner.is_empty() {
                    // `M()`: a member of the caller's own type.
                    caller.path().split_last().map(|(_, type_path)| {
                        NodeId::within(
                            Level::Code,
                            system.clone(),
                            [type_path, std::slice::from_ref(name)].concat(),
                        )
                    })
                } else {
                    self.cs_type(system, &at, owner, &usings).map(|(s, ns, t)| {
                        NodeId::within(Level::Code, s, [ns, vec![t, name.clone()]].concat())
                    })
                };
                if let Some(callee) = callee.filter(|c| self.model.node(c).is_some())
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
