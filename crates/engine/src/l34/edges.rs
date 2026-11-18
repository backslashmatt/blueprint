//! Edges from the analysers' references (#62, §3.2), and their roll-up.
//!
//! - **depends-on (L2):** each `use` in a module, resolved through `crate`,
//!   `self`, `super`, the module's own submodules, the crate root (2015
//!   paths) and the workspace's other crates. `exact` when the named item or
//!   module exists, `inferred` when only its module does. Paths into std and
//!   external crates give no edge: they are outside the model.
//! - **implements (L3):** `impl Trait for Type` where both resolve to L3
//!   nodes, through the module's own items and its `use` imports.
//! - **Roll-up:** every edge between nodes with different parents adds the
//!   same edge between the parents, with the lower edges as evidence, up to
//!   L1. A roll-up is as sure as its surest evidence.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use blueprint_model::lang::Reference;
use blueprint_model::{
    Confidence, Ecosystem, Edge, EdgeKey, EdgeKind, Level, NodeId, NodeKind, RepoPath, SystemKey,
};

use super::{Builder, FUNCTIONS, Parsed};

/// The names each module binds with `use`: (system, module) -> name -> path.
type Imports = HashMap<(SystemKey, Vec<String>), HashMap<String, Vec<String>>>;

/// What a path resolved to: a module, and the item named in it, if any.
struct Resolved {
    system: SystemKey,
    module: Vec<String>,
    /// The first segment after the module, if the path goes past it.
    item: Option<String>,
    /// Whether that item (or, with no item, the module) exists.
    found: bool,
}

pub(super) fn add(b: &mut Builder<'_>, parsed: &[Parsed<'_>]) {
    let crates: HashMap<String, SystemKey> = b
        .model
        .level(Level::System)
        .filter(|n| n.id.system_key().ecosystem == Ecosystem::Cargo)
        .map(|n| (n.name.replace('-', "_"), n.id.system_key().clone()))
        .collect();
    // Names bound by `use` in each module: name -> path. Only `impl`
    // resolution needs them, so only modules with an `impl` are recorded.
    let mut imports: Imports = HashMap::new();
    for (_, place, file_module, facts) in parsed {
        let impl_modules: HashSet<&Vec<String>> = facts
            .references
            .iter()
            .filter_map(|r| match r {
                Reference::Impl { module, .. } => Some(module),
                Reference::Use { .. } => None,
            })
            .collect();
        if impl_modules.is_empty() {
            continue;
        }
        for r in &facts.references {
            if let Reference::Use {
                path,
                alias,
                module,
            } = r
            {
                if !impl_modules.contains(module) {
                    continue;
                }
                let bound = alias.clone().or_else(|| path.last().cloned());
                if let Some(bound) = bound.filter(|n| n != "*" && n != "self") {
                    let m = [file_module.as_slice(), module.as_slice()].concat();
                    imports
                        .entry((place.system.clone(), m))
                        .or_default()
                        .insert(bound, path.clone());
                }
            }
        }
    }
    let index = Index::build(b.model);
    let r = Resolver {
        b: &*b,
        crates: &crates,
        imports: &imports,
        index: &index,
    };
    let mut found: Vec<(EdgeKey, Confidence, RepoPath)> = Vec::new();
    for (path, place, file_module, facts) in parsed {
        let system = &place.system;
        for reference in &facts.references {
            match reference {
                Reference::Use {
                    path: p, module, ..
                } => {
                    let m = [file_module.as_slice(), module.as_slice()].concat();
                    let Some(to) = r.resolve(system, &m, p, false) else {
                        continue;
                    };
                    let conf = if to.found {
                        Confidence::Exact
                    } else {
                        Confidence::Inferred
                    };
                    let from = NodeId::within(Level::Module, system.clone(), m);
                    let to_id = NodeId::within(Level::Module, to.system, to.module);
                    let key = EdgeKey {
                        from,
                        to: to_id,
                        kind: EdgeKind::DependsOn,
                    };
                    found.push((key, conf, (*path).clone()));
                }
                Reference::Impl {
                    self_type,
                    r#trait,
                    module,
                } => {
                    let m = [file_module.as_slice(), module.as_slice()].concat();
                    let ty = r.item(system, &m, self_type);
                    let tr = r.item(system, &m, r#trait);
                    if let (Some(from), Some(to)) = (ty, tr) {
                        let key = EdgeKey {
                            from,
                            to,
                            kind: EdgeKind::Implements,
                        };
                        found.push((key, Confidence::Exact, (*path).clone()));
                    }
                }
            }
        }
    }
    // Many uses name the same module: keep each edge once, surest first.
    let mut unique: BTreeMap<EdgeKey, (Confidence, RepoPath)> = BTreeMap::new();
    for (key, conf, path) in found {
        match unique.get_mut(&key) {
            Some(seen) if seen.0 >= conf => {}
            Some(seen) => *seen = (conf, path),
            None => {
                unique.insert(key, (conf, path));
            }
        }
    }
    for (key, (conf, path)) in unique {
        if key.from == key.to {
            continue;
        }
        // A crate root's L2 node exists only once something needs it.
        for end in [&key.from, &key.to] {
            if end.level() == Level::Module && end.path().is_empty() {
                b.module(end.system_key(), &[], &path);
            }
        }
        if b.model.node(&key.from).is_none() || b.model.node(&key.to).is_none() {
            continue;
        }
        let edge = Edge {
            key,
            confidence: conf,
            evidence: BTreeSet::new(),
            dependency: BTreeSet::new(),
        };
        if let Err(e) = b.model.add_edge(edge) {
            b.diags.push(crate::l1::Diagnostic {
                path,
                message: e.to_string(),
            });
        }
    }
    roll_up(b);
}

struct Resolver<'a, 'b> {
    b: &'a Builder<'b>,
    crates: &'a HashMap<String, SystemKey>,
    imports: &'a Imports,
    index: &'a Index,
}

/// What resolution looks up, per system: the module paths, and the names
/// of the types and free functions in each module. Built once, so that a
/// probe is one hash lookup instead of building and comparing node ids.
#[derive(Default)]
struct Index {
    modules: HashMap<SystemKey, HashSet<Vec<String>>>,
    items: HashMap<SystemKey, HashMap<Vec<String>, HashSet<String>>>,
}

impl Index {
    /// Records `name` in `module`, cloning keys only when they are new.
    fn add_item(&mut self, sys: &SystemKey, module: &[String], name: &str) {
        if !self.items.contains_key(sys) {
            self.items.insert(sys.clone(), HashMap::new());
        }
        let Some(by_module) = self.items.get_mut(sys) else {
            return;
        };
        if let Some(names) = by_module.get_mut(module) {
            names.insert(name.to_owned());
        } else {
            by_module.insert(module.to_vec(), HashSet::from([name.to_owned()]));
        }
    }

    fn build(model: &blueprint_model::Model) -> Index {
        let mut index = Index::default();
        for n in model.nodes() {
            let sys = n.id.system_key();
            let path = n.id.path();
            match (n.id.level(), n.kind) {
                (Level::Module, NodeKind::Module) => {
                    if let Some(m) = index.modules.get_mut(sys) {
                        m.insert(path.to_vec());
                    } else {
                        index
                            .modules
                            .insert(sys.clone(), HashSet::from([path.to_vec()]));
                    }
                }
                (Level::Component, _) => {
                    if let Some((name, module)) = path.split_last() {
                        index.add_item(sys, module, name);
                    }
                }
                (Level::Code, _) => {
                    if let [module @ .., group, name] = path
                        && group == FUNCTIONS
                    {
                        index.add_item(sys, module, name);
                    }
                }
                _ => {}
            }
        }
        index
    }
}

impl Resolver<'_, '_> {
    fn is_module(&self, system: &SystemKey, path: &[String]) -> bool {
        path.is_empty()
            || self
                .index
                .modules
                .get(system)
                .is_some_and(|m| m.contains(path))
    }

    /// Whether `module` is followed by `name` as a module.
    fn is_child(&self, system: &SystemKey, module: &mut Vec<String>, name: &str) -> bool {
        module.push(name.to_owned());
        let yes = self.is_module(system, module);
        module.pop();
        yes
    }

    fn has_item(&self, system: &SystemKey, module: &[String], name: &str) -> bool {
        self.index
            .items
            .get(system)
            .and_then(|m| m.get(module))
            .is_some_and(|names| names.contains(name))
    }

    /// Resolves `path`, written in module `m` of `system`, to a module and
    /// the item after it. `None` for paths outside the workspace. With
    /// `imported`, a first segment bound by a `use` in `m` stands for its
    /// path (once, so imports cannot loop).
    fn resolve(
        &self,
        system: &SystemKey,
        m: &[String],
        path: &[String],
        imported: bool,
    ) -> Option<Resolved> {
        let mut segs: Vec<String> = path.to_vec();
        if segs.last().is_some_and(|s| s == "*") {
            segs.pop();
        }
        let root: Vec<String> = match m.first() {
            Some(first) if first.starts_with("(bin ") => vec![first.clone()],
            _ => Vec::new(),
        };
        let first = segs.first()?.clone();
        let (sys, mut cur, rest) = match first.as_str() {
            "crate" => (system.clone(), root, 1),
            "self" => (system.clone(), m.to_vec(), 1),
            "super" => {
                let mut cur = m.to_vec();
                let mut n = 0;
                while segs.get(n).is_some_and(|s| s == "super") {
                    if cur.len() <= root.len() {
                        return None;
                    }
                    cur.pop();
                    n += 1;
                }
                (system.clone(), cur, n)
            }
            _ if imported
                && let Some(target) = self
                    .imports
                    .get(&(system.clone(), m.to_vec()))
                    .and_then(|i| i.get(&first)) =>
            {
                let mut whole = target.clone();
                whole.extend(segs[1..].iter().cloned());
                return self.resolve(system, m, &whole, false);
            }
            s if self.is_child(system, &mut m.to_vec(), s) => (system.clone(), m.to_vec(), 0),
            s if self.is_child(system, &mut root.clone(), s) => (system.clone(), root, 0),
            s => (self.crates.get(s)?.clone(), Vec::new(), 1),
        };
        let mut i = rest;
        while let Some(seg) = segs.get(i) {
            if !self.is_child(&sys, &mut cur, seg) {
                break;
            }
            cur.push(seg.clone());
            i += 1;
        }
        let item = segs.get(i).cloned();
        let found = match &item {
            None => true,
            Some(name) => self.has_item(&sys, &cur, name),
        };
        Some(Resolved {
            system: sys,
            module: cur,
            item,
            found,
        })
    }

    /// The L3 node a type or trait path names, written in module `m`.
    fn item(&self, system: &SystemKey, m: &[String], path: &[String]) -> Option<NodeId> {
        let local = || {
            let [name] = path else { return None };
            let mut p = m.to_vec();
            p.push(name.clone());
            let id = NodeId::within(Level::Component, system.clone(), p);
            self.b.model.node(&id).map(|_| id)
        };
        local().or_else(|| {
            let r = self.resolve(system, m, path, true)?;
            let name = r.item.filter(|_| r.found)?;
            let mut p = r.module;
            p.push(name);
            let id = NodeId::within(Level::Component, r.system, p);
            self.b.model.node(&id).map(|_| id)
        })
    }
}

/// Adds, level by level from L4 up, the roll-up of every edge whose ends
/// have different parents.
fn roll_up(b: &mut Builder<'_>) {
    for level in [Level::Code, Level::Component, Level::Module] {
        let mut up: BTreeMap<EdgeKey, (Confidence, BTreeSet<EdgeKey>)> = BTreeMap::new();
        for e in b.model.edges().filter(|e| e.key.from.level() == level) {
            let parent = |id: &NodeId| b.model.node(id).and_then(|n| n.parent.clone());
            let (Some(from), Some(to)) = (parent(&e.key.from), parent(&e.key.to)) else {
                continue;
            };
            if from == to {
                continue;
            }
            let key = EdgeKey {
                from,
                to,
                kind: e.key.kind,
            };
            let entry = up
                .entry(key)
                .or_insert((Confidence::Unknown, BTreeSet::new()));
            entry.0 = entry.0.max(e.confidence);
            entry.1.insert(e.key.clone());
        }
        for (key, (confidence, evidence)) in up {
            let edge = Edge {
                key,
                confidence,
                evidence,
                dependency: BTreeSet::new(),
            };
            if let Err(e) = b.model.add_edge(edge) {
                b.diags.push(crate::l1::Diagnostic {
                    path: RepoPath::root(),
                    message: e.to_string(),
                });
            }
        }
    }
}
