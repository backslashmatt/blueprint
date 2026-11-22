//! Edges from C# references (#150, DR 0013).
//!
//! A name resolves in the project's own namespaces and those of the
//! projects it references (its L1 edges), as the compiler sees them;
//! external namespaces (`System.*`, packages) give no edge.
//!
//! - **depends-on (L2):** `using A.B;` (and `using static A.T;`, `using X =
//!   A.B;`) from each namespace of the file's items that the `using` is in
//!   scope for, to the namespace it names, `exact`.
//! - **extends / implements (L3):** each base of a type, looked up as C#
//!   does: the enclosing types' nested types; then for each namespace from
//!   the type's outwards, its types, then that declaration's own aliases and
//!   `using`s; or as a qualified name. `implements` when a class, struct or
//!   record names an interface, `extends` otherwise (an interface extends
//!   the interfaces it names). A base that does not resolve, or that names
//!   more than one type (`Base` and generic `Base<T>`), gives no edge.
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

    /// The namespaces brought in by `using`s declared exactly at `ns`.
    fn globs_here<'s>(&'s self, ns: &'s [String]) -> impl Iterator<Item = &'f [String]> + 's {
        self.globs
            .iter()
            .filter(move |(m, _)| *m == ns)
            .map(|(_, b)| *b)
    }

    /// An alias declared exactly at `ns`.
    fn alias_here(&self, ns: &[String], name: &str) -> Option<&'f [String]> {
        self.aliases
            .iter()
            .find(|(m, a, _)| *a == name && *m == ns)
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

    /// A type `name` in namespace `ns` of a visible project. A name more
    /// than one type has (`Base` and generic `Base<T>`, which the analyser
    /// cannot tell apart, so `Base~2`) is ambiguous and resolves to none.
    fn cs_find(
        &self,
        system: &SystemKey,
        ns: &[String],
        name: &str,
    ) -> Option<Option<(SystemKey, Vec<String>, String)>> {
        let mut found = None;
        for sys in self.projects.visible(system) {
            if self.has_item(sys, ns, name) {
                if found.is_some() || self.has_item(sys, ns, &format!("{name}~2")) {
                    return Some(None);
                }
                found = Some((sys.clone(), ns.to_vec(), name.to_owned()));
            }
        }
        found.map(Some)
    }

    /// The type a (possibly qualified) name used in namespace `at`, inside
    /// type `within` (`Outer.Sub`, or `None`), names: its system, namespace
    /// and name. Looked up as C# does: the enclosing types; then, for each
    /// namespace from `at` outwards, its types and then that declaration's
    /// own `using`s and aliases; finally as a fully qualified name.
    fn cs_type(
        &self,
        system: &SystemKey,
        at: &[String],
        within: Option<&str>,
        path: &[String],
        usings: &Usings<'_>,
    ) -> Option<(SystemKey, Vec<String>, String)> {
        let (first, rest) = path.split_first()?;
        let dotted = path.join(".");
        // Nested types of the enclosing types, innermost first.
        if let Some(within) = within {
            let outer: Vec<&str> = within.split('.').collect();
            for n in (1..=outer.len()).rev() {
                let name = format!("{}.{dotted}", outer[..n].join("."));
                if let Some(hit) = self.cs_find(system, at, &name) {
                    return hit;
                }
            }
        }
        for level in (0..=at.len()).rev() {
            let ns = &at[..level];
            // A type of this namespace (`T`, or `Outer.Inner`).
            if let Some(hit) = self.cs_find(system, ns, &dotted) {
                return hit;
            }
            // A namespace inside it: `Sub.T` from `ns`.
            if !rest.is_empty()
                && let Some(hit) = self.qualified(system, &[ns, path].concat())
            {
                return Some(hit);
            }
            // This declaration's own aliases, then its usings.
            if let Some(target) = usings.alias_here(ns, first) {
                return self.qualified(system, &[target, rest].concat());
            }
            for base in usings.globs_here(ns) {
                if let Some(hit) = self.cs_find(system, base, &dotted) {
                    return hit;
                }
            }
        }
        None
    }

    /// `A.B.T` (or `A.B.Outer.Inner`) as a namespace and a type in it.
    fn qualified(
        &self,
        system: &SystemKey,
        path: &[String],
    ) -> Option<(SystemKey, Vec<String>, String)> {
        for split in (0..path.len()).rev() {
            let (ns, name) = (&path[..split], path[split..].join("."));
            if !self
                .projects
                .visible(system)
                .any(|s| self.is_namespace(s, ns))
            {
                continue;
            }
            if let Some(hit) = self.cs_find(system, ns, &name) {
                return hit;
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
                    let Some((sys, ns, name)) =
                        self.cs_type(system, &at, Some(ty), r#trait, &usings)
                    else {
                        continue;
                    };
                    let interface =
                        self.projects
                            .interfaces
                            .contains(&(sys.clone(), ns.clone(), name.clone()));
                    // An interface's bases are interfaces it extends.
                    let from_interface = self.projects.interfaces.contains(&(
                        system.clone(),
                        at.clone(),
                        ty.clone(),
                    ));
                    let to = NodeId::within(Level::Component, sys, [ns, vec![name]].concat());
                    if self.model.node(&to).is_some() {
                        let kind = if interface && !from_interface {
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
            let (Some(caller), ItemKind::Method { self_type, .. }) = (node(i), &item.kind) else {
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
                    self.cs_type(system, &at, Some(self_type), owner, &usings)
                        .map(|(s, ns, t)| {
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
