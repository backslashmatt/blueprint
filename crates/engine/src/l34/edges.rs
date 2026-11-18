//! Edges from the analysers' references (#62, §3.2), and their roll-up.
//!
//! - **depends-on (L2):** each `use` in a module, resolved through `crate`,
//!   `self`, `super`, names the module binds with `use … as`, the module's
//!   own submodules, the crates this crate declares in its manifest (by the
//!   name it uses for them, renames and `[lib] name` included), and finally
//!   the crate root (2015 paths). `exact` when the named item or module
//!   exists, `inferred` when only its module does. Crates it does not
//!   declare — std, external crates, unrelated workspace members — give no
//!   edge.
//! - **implements (L3):** `impl Trait for Type` where both resolve to L3
//!   nodes and the impl is in the type's crate, so the edge runs the way
//!   the dependency does. An impl of a local trait for a foreign type gives
//!   no edge.
//! - **Roll-up:** every edge between nodes with different parents adds the
//!   same edge between the parents, with the lower edges as evidence, as
//!   sure as its surest evidence. At L1 it only adds evidence to edges the
//!   manifests declare: a crate used only by tests (a dev-dependency) or not
//!   declared at all does not become a system dependency.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use blueprint_model::lang::Reference;
use blueprint_model::{
    Confidence, Ecosystem, Edge, EdgeKey, EdgeKind, Level, NodeId, NodeKind, RepoPath, SystemKey,
};

use toml::{Table, Value};

use super::{Builder, FUNCTIONS, Parsed};
use crate::l1::Read;

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
    /// Whether the item is the path's last segment (nothing follows it).
    item_tail_is_last: bool,
}

/// The crates each Cargo system can name: extern name -> system.
type Crates = HashMap<SystemKey, HashMap<String, SystemKey>>;

/// For each Cargo system, the crates its manifest declares, by the name its
/// code uses for them, and its own library name (for its binaries).
fn crate_names(b: &Builder<'_>, read: Read<'_>) -> Crates {
    let systems: Vec<(SystemKey, String, Table)> = b
        .model
        .level(Level::System)
        .filter(|n| n.id.system_key().ecosystem == Ecosystem::Cargo)
        .map(|n| {
            let key = n.id.system_key().clone();
            let mut manifest = key.dir.segments().to_vec();
            manifest.push("Cargo.toml".into());
            let table: Table = RepoPath::parse(&manifest.join("/"))
                .ok()
                .and_then(|p| read(&p))
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|text| text.parse().ok())
                .unwrap_or_default();
            (key, n.name.clone(), table)
        })
        .collect();
    let lib_name = |package: &str, table: &Table| {
        table
            .get("lib")
            .and_then(|l| l.get("name"))
            .and_then(Value::as_str)
            .map_or_else(|| package.replace('-', "_"), str::to_owned)
    };
    // (system, package name, library name) for every Cargo system.
    let info: Vec<(&SystemKey, &str, String)> = systems
        .iter()
        .map(|(k, name, t)| (k, name.as_str(), lib_name(name, t)))
        .collect();
    let mut out = Crates::new();
    for (key, name, table) in &systems {
        let names = out.entry(key.clone()).or_default();
        names.insert(lib_name(name, table), key.clone());
        // What Cargo resolved: the L1 analyser's edges from this system.
        let resolved: Vec<&SystemKey> = b
            .model
            .edges()
            .filter(|e| {
                e.key.kind == EdgeKind::DependsOn
                    && e.key.from.level() == Level::System
                    && e.key.from.system_key() == key
            })
            .map(|e| e.key.to.system_key())
            .collect();
        for section in ["dependencies", "build-dependencies", "dev-dependencies"] {
            let deps = table
                .get(section)
                .and_then(Value::as_table)
                .into_iter()
                .flatten();
            for (dep, spec) in deps {
                let renamed = spec.get("package").and_then(Value::as_str);
                let package = renamed.unwrap_or(dep);
                let target = if section == "dev-dependencies" {
                    // Cargo's graph leaves dev-dependencies out, so only a
                    // `path` into the workspace identifies one.
                    let dir = spec
                        .get("path")
                        .and_then(Value::as_str)
                        .and_then(|p| crate::l1::path::normalise(key.dir.segments(), p));
                    info.iter()
                        .find(|(k, n, _)| *n == package && dir.as_deref() == Some(k.dir.segments()))
                } else {
                    info.iter()
                        .find(|(k, n, _)| *n == package && resolved.contains(k))
                };
                let Some((target, _, lib)) = target else {
                    continue;
                };
                let extern_name = match renamed {
                    Some(_) => dep.replace('-', "_"),
                    None => lib.clone(),
                };
                names.insert(extern_name, (*target).clone());
            }
        }
    }
    out
}

pub(super) fn add(
    b: &mut Builder<'_>,
    parsed: &[Parsed<'_>],
    functions: &HashMap<(usize, usize), NodeId>,
    read: Read<'_>,
) {
    let crates = crate_names(b, read);
    // Names bound by `use` in each module: name -> path. `impl` resolution
    // needs every one in its module; use resolution needs only renames and
    // single-segment imports (`use a as b`, `use some_crate`), which can
    // start a later path. Recording only those keeps this cheap.
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
        for r in &facts.references {
            if let Reference::Use {
                path,
                alias,
                module,
            } = r
            {
                if alias.is_none() && path.len() > 1 && !impl_modules.contains(module) {
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
    for (f, (path, place, file_module, facts)) in parsed.iter().enumerate() {
        let system = &place.system;
        // What each module of this file imports, for its calls.
        let mut local: HashMap<&[String], HashMap<&str, &[String]>> = HashMap::new();
        for reference in &facts.references {
            if let Reference::Use {
                path,
                alias,
                module,
            } = reference
            {
                let bound = alias.as_deref().or_else(|| path.last().map(String::as_str));
                if let Some(bound) = bound.filter(|n| *n != "*" && *n != "self") {
                    local
                        .entry(module.as_slice())
                        .or_default()
                        .insert(bound, path.as_slice());
                }
            }
        }
        for (i, item) in facts.items.iter().enumerate() {
            let Some(caller) = functions.get(&(f, i)) else {
                continue;
            };
            let m = [file_module.as_slice(), item.module.as_slice()].concat();
            let imported = local.get(item.module.as_slice());
            for call in &item.calls {
                if let Some(callee) = r.callee(system, &m, caller, call, imported)
                    && callee != *caller
                {
                    let key = EdgeKey {
                        from: caller.clone(),
                        to: callee,
                        kind: EdgeKind::Calls,
                    };
                    found.push((key, Confidence::Inferred, (*path).clone()));
                }
            }
        }
        for reference in &facts.references {
            match reference {
                Reference::Use {
                    path: p, module, ..
                } => {
                    let m = [file_module.as_slice(), module.as_slice()].concat();
                    let Some(to) = r.resolve(system, &m, p, true) else {
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
                    // Only an impl in the type's own crate: then the edge
                    // runs the way the dependency does.
                    if let (Some(from), Some(to)) = (ty, tr)
                        && from.system_key() == system
                    {
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
    crates: &'a Crates,
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
            // A declared crate wins over a same-named root module (2018+).
            s if let Some(target) = self.crates.get(system).and_then(|c| c.get(s)) => {
                (target.clone(), Vec::new(), 1)
            }
            s if self.is_child(system, &mut root.clone(), s) => (system.clone(), root, 0),
            _ => return None,
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
        let item_tail_is_last = i + 1 == segs.len();
        let found = match &item {
            None => true,
            Some(name) => self.has_item(&sys, &cur, name),
        };
        Some(Resolved {
            system: sys,
            module: cur,
            item,
            found,
            item_tail_is_last,
        })
    }

    /// The L4 function a call path names, from `caller` in module `m`:
    /// a free function in `m` or imported into it, `module::f`, an inherent
    /// method `Type::f`, or `Self::f` on the caller's own type. Calls are
    /// resolved by name alone (a local closure can shadow them), so the
    /// edges are `inferred`.
    fn callee(
        &self,
        system: &SystemKey,
        m: &[String],
        caller: &NodeId,
        call: &[String],
        imported: Option<&HashMap<&str, &[String]>>,
    ) -> Option<NodeId> {
        let model = &self.b.model;
        let at = |sys: &SystemKey, mut path: Vec<String>| {
            let id = NodeId::within(Level::Code, sys.clone(), {
                path.shrink_to_fit();
                path
            });
            model.node(&id).map(|_| id)
        };
        let free = |sys: &SystemKey, module: &[String], name: &str| {
            at(
                sys,
                [module, &[FUNCTIONS.to_owned(), name.to_owned()]].concat(),
            )
        };
        let (name, prefix) = call.split_last()?;
        let import = |first: &str| imported.and_then(|i| i.get(first)).copied();
        if prefix.is_empty() {
            if let Some(id) = free(system, m, name) {
                return Some(id);
            }
            let target = import(name)?;
            let r = self.resolve(system, m, target, false)?;
            return free(&r.system, &r.module, r.item.as_deref()?);
        }
        if prefix == ["Self"] {
            let ty = model.node(caller)?.parent.clone()?;
            return at(
                ty.system_key(),
                [ty.path(), std::slice::from_ref(name)].concat(),
            );
        }
        let mut whole: Vec<String> = match import(&prefix[0]) {
            Some(target) => [target, &prefix[1..]].concat(),
            None => prefix.to_vec(),
        };
        if let Some(r) = self.resolve(system, m, &whole, false) {
            match r.item {
                // `module::f`
                None => return free(&r.system, &r.module, name),
                // `module::Type::f`
                Some(ty) if r.found && r.item_tail_is_last => {
                    return at(&r.system, [r.module, vec![ty, name.clone()]].concat());
                }
                _ => {}
            }
        }
        // `Type::f` with `Type` declared or imported here.
        whole = prefix.to_vec();
        let ty = self.item(system, m, &whole)?;
        at(
            ty.system_key(),
            [ty.path(), std::slice::from_ref(name)].concat(),
        )
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
            // Systems depend on each other only where a manifest says so.
            if key.from.level() == Level::System && b.model.edge(&key).is_none() {
                continue;
            }
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
