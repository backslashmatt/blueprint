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

use blueprint_model::lang::FileFacts;
use blueprint_model::lang::Reference;
use blueprint_model::{
    Confidence, Ecosystem, Edge, EdgeKey, EdgeKind, Level, Model, NodeId, NodeKind, RepoPath,
    SystemKey,
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

/// What resolving references needs besides the model, kept so that one
/// file's calls can be resolved again ([`file_calls`]).
pub(crate) struct Context {
    crates: Crates,
    imports: Imports,
    index: Index,
}

/// Adds every edge the facts give, rolled up. With `keep`, also returns the
/// resolution context and each file's direct edges (by its index in
/// `parsed`), for a resident engine.
pub(super) fn add(
    b: &mut Builder<'_>,
    parsed: &[Parsed<'_>],
    functions: &HashMap<(usize, usize), NodeId>,
    read: Read<'_>,
    keep: bool,
) -> Option<(Context, Vec<Contributions>)> {
    let crates = crate_names(b, read);
    // Names bound by `use` in each module: name -> path. `impl` resolution
    // needs every one in its module; use resolution needs only renames and
    // single-segment imports (`use a as b`, `use some_crate`), which can
    // start a later path. Recording only those keeps this cheap.
    let mut imports: Imports = HashMap::new();
    for (_, place, file_module, facts) in parsed {
        add_imports(&mut imports, &place.system, file_module, facts);
    }
    let index = Index::build(b.model);
    let r = Resolver {
        model: &*b.model,
        crates: &crates,
        imports: &imports,
        index: &index,
    };
    let mut found: Vec<(EdgeKey, Confidence, RepoPath)> = Vec::new();
    let mut kept = Vec::with_capacity(if keep { parsed.len() } else { 0 });
    for (f, (path, place, file_module, facts)) in parsed.iter().enumerate() {
        let mine = r.file_edges(&place.system, file_module, facts, |i| {
            functions.get(&(f, i))
        });
        if keep {
            kept.push(contributions(&mine));
        }
        found.extend(mine.into_iter().map(|(k, c)| (k, c, (*path).clone())));
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
    keep.then_some((
        Context {
            crates,
            imports,
            index,
        },
        kept,
    ))
}

/// One file's direct edges, each with the surest confidence the file gives
/// it. Edges from a node to itself are left out, as [`add`] leaves them out.
pub(crate) type Contributions = BTreeMap<EdgeKey, Confidence>;

fn contributions(found: &[(EdgeKey, Confidence)]) -> Contributions {
    let mut out = Contributions::new();
    for (k, c) in found {
        if k.from == k.to {
            continue;
        }
        let e = out.entry(k.clone()).or_insert(*c);
        *e = (*e).max(*c);
    }
    out
}

/// Records the names `facts`' `use`s bind in each of the file's modules.
/// `impl` resolution needs every one in its module; use resolution needs
/// only renames and single-segment imports (`use a as b`, `use some_crate`),
/// which can start a later path. Recording only those keeps this cheap.
fn add_imports(
    imports: &mut Imports,
    system: &SystemKey,
    file_module: &[String],
    facts: &FileFacts,
) {
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
                let m = [file_module, module.as_slice()].concat();
                imports
                    .entry((system.clone(), m))
                    .or_default()
                    .insert(bound, path.clone());
            }
        }
    }
}

/// The modules a file's facts can bind names in: the file's own and its
/// inline modules.
fn file_modules(
    system: &SystemKey,
    file_module: &[String],
    facts: &FileFacts,
) -> BTreeSet<(SystemKey, Vec<String>)> {
    let mut out = BTreeSet::from([(system.clone(), file_module.to_vec())]);
    for r in &facts.references {
        let module = match r {
            Reference::Use { module, .. } | Reference::Impl { module, .. } => module,
        };
        out.insert((system.clone(), [file_module, module.as_slice()].concat()));
    }
    out
}

impl Context {
    /// Replaces the names a file's `use`s bind: `old` facts out, `new` in.
    pub(crate) fn rebind(
        &mut self,
        system: &SystemKey,
        file_module: &[String],
        old: &FileFacts,
        new: &FileFacts,
    ) {
        for m in file_modules(system, file_module, old) {
            self.imports.remove(&m);
        }
        add_imports(&mut self.imports, system, file_module, new);
    }

    /// Replaces the names a file's items give its modules in the index.
    /// `old` and `new` are each module's type and free-function names.
    pub(crate) fn rename(
        &mut self,
        system: &SystemKey,
        old: &BTreeSet<(Vec<String>, String)>,
        new: &BTreeSet<(Vec<String>, String)>,
    ) {
        let Some(by_module) = self.index.items.get_mut(system) else {
            if new.is_empty() {
                return;
            }
            for (m, n) in new {
                self.index.add_item(system, m, n);
            }
            return;
        };
        for (m, n) in old.difference(new) {
            if let Some(names) = by_module.get_mut(m) {
                names.remove(n);
            }
        }
        for (m, n) in new.difference(old) {
            self.index.add_item(system, m, n);
        }
    }
}

/// One file's direct edges, resolved against `model` and `ctx`. `node`
/// gives the L4 node of each item, by its index in the facts.
pub(crate) fn edges_of<'n>(
    model: &Model,
    ctx: &Context,
    system: &SystemKey,
    file_module: &[String],
    facts: &FileFacts,
    node: impl Fn(usize) -> Option<&'n NodeId>,
) -> Contributions {
    let r = Resolver {
        model,
        crates: &ctx.crates,
        imports: &ctx.imports,
        index: &ctx.index,
    };
    contributions(&r.file_edges(system, file_module, facts, node))
}

/// The `calls` edges of one file's functions, resolved against `model` and
/// `ctx`. `node` gives the L4 node of each item, by its index in the facts.
pub(crate) fn calls_of<'n>(
    model: &Model,
    ctx: &Context,
    system: &SystemKey,
    file_module: &[String],
    facts: &FileFacts,
    node: impl Fn(usize) -> Option<&'n NodeId>,
) -> BTreeSet<EdgeKey> {
    let r = Resolver {
        model,
        crates: &ctx.crates,
        imports: &ctx.imports,
        index: &ctx.index,
    };
    r.file_calls(system, file_module, facts, node)
        .into_iter()
        .collect()
}

/// Adds the direct edge `key` and rolls it up, as [`roll_up`] would: the
/// same edge between the parents, with `key` as evidence, up to L2, and at
/// L1 only into an edge that already exists.
pub(crate) fn add_rolled(model: &mut Model, key: EdgeKey, confidence: Confidence) {
    let mut evidence = BTreeSet::new();
    let mut key = key;
    loop {
        let parents = parents(model, &key);
        let edge = Edge {
            key: key.clone(),
            confidence,
            evidence: std::mem::take(&mut evidence),
            dependency: BTreeSet::new(),
        };
        if model.add_edge(edge).is_err() {
            return;
        }
        let Some(up) = parents else {
            return;
        };
        if up.from.level() == Level::System && model.edge(&up).is_none() {
            return;
        }
        evidence.insert(key);
        key = up;
    }
}

/// Removes the direct edge `key` and its roll-up: it leaves the evidence
/// of the edge above, which goes too once it has no evidence left and was
/// not found directly. A rolled-up edge keeps the surest of its evidence;
/// an L1 edge, which the manifests declared, keeps at least the confidence
/// `l1` records it was declared with.
pub(crate) fn remove_rolled(model: &mut Model, key: &EdgeKey, l1: &HashMap<EdgeKey, Confidence>) {
    let up = parents(model, key);
    if !model.remove_edge(key) {
        return;
    }
    let Some(up) = up else {
        return;
    };
    let Some(mut above) = model.edge(&up).cloned() else {
        return;
    };
    above.evidence.remove(key);
    let declared = up.from.level() == Level::System;
    if above.evidence.is_empty() && !declared {
        remove_rolled(model, &up, l1);
        return;
    }
    let surest = above
        .evidence
        .iter()
        .filter_map(|k| model.edge(k).map(|e| e.confidence))
        .max();
    above.confidence = match (declared, surest) {
        (true, s) => l1
            .get(&up)
            .copied()
            .into_iter()
            .chain(s)
            .max()
            .unwrap_or(above.confidence),
        (false, Some(s)) => s,
        (false, None) => above.confidence,
    };
    model.remove_edge(&up);
    let _ = model.add_edge(above);
}

/// The edge `key` stands for one level up, if its ends have different
/// parents.
fn parents(model: &Model, key: &EdgeKey) -> Option<EdgeKey> {
    let parent = |id: &NodeId| model.node(id).and_then(|n| n.parent.clone());
    let (from, to) = (parent(&key.from)?, parent(&key.to)?);
    (from != to).then_some(EdgeKey {
        from,
        to,
        kind: key.kind,
    })
}

struct Resolver<'a> {
    model: &'a Model,
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

impl Resolver<'_> {
    /// One file's direct edges: its functions' calls, its modules' uses and
    /// its impls, each with its confidence.
    fn file_edges<'n>(
        &self,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<(EdgeKey, Confidence)> {
        let mut found: Vec<(EdgeKey, Confidence)> = self
            .file_calls(system, file_module, facts, node)
            .into_iter()
            .map(|k| (k, Confidence::Inferred))
            .collect();
        for reference in &facts.references {
            match reference {
                Reference::Use {
                    path: p, module, ..
                } => {
                    let m = [file_module, module.as_slice()].concat();
                    let Some(to) = self.resolve(system, &m, p, true) else {
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
                    found.push((key, conf));
                }
                Reference::Impl {
                    self_type,
                    r#trait,
                    module,
                } => {
                    let m = [file_module, module.as_slice()].concat();
                    let ty = self.item(system, &m, self_type);
                    let tr = self.item(system, &m, r#trait);
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
                        found.push((key, Confidence::Exact));
                    }
                }
            }
        }
        found
    }

    /// The `calls` edges of one file's functions. `node` gives the L4 node
    /// of each item, by its index in the facts.
    fn file_calls<'n>(
        &self,
        system: &SystemKey,
        file_module: &[String],
        facts: &FileFacts,
        node: impl Fn(usize) -> Option<&'n NodeId>,
    ) -> Vec<EdgeKey> {
        // What each module of this file imports, for its calls.
        let mut local: HashMap<&[String], HashMap<&str, &[String]>> = HashMap::new();
        let mut globs: HashMap<&[String], Vec<&[String]>> = HashMap::new();
        for reference in &facts.references {
            if let Reference::Use {
                path,
                alias,
                module,
            } = reference
            {
                if let Some((star, base)) = path.split_last()
                    && star == "*"
                {
                    globs.entry(module.as_slice()).or_default().push(base);
                }
                let bound = alias.as_deref().or_else(|| path.last().map(String::as_str));
                if let Some(bound) = bound.filter(|n| *n != "*" && *n != "self") {
                    local
                        .entry(module.as_slice())
                        .or_default()
                        .insert(bound, path.as_slice());
                }
            }
        }
        let mut out = Vec::new();
        for (i, item) in facts.items.iter().enumerate() {
            let Some(caller) = node(i) else {
                continue;
            };
            let m = [file_module, item.module.as_slice()].concat();
            let imported = local.get(item.module.as_slice());
            let glob = globs
                .get(item.module.as_slice())
                .map_or(&[][..], Vec::as_slice);
            for call in &item.calls {
                if let Some(callee) = self.callee(system, &m, caller, call, imported, glob)
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
        globs: &[&[String]],
    ) -> Option<NodeId> {
        let model = &self.model;
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
            if let Some(target) = import(name) {
                let r = self.resolve(system, m, target, false)?;
                return free(&r.system, &r.module, r.item.as_deref()?);
            }
            // `use super::*` and other globs: the first module that has it.
            return globs.iter().find_map(|base| {
                let r = self.resolve(system, m, base, false)?;
                r.item.is_none().then(|| free(&r.system, &r.module, name))?
            });
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
            self.model.node(&id).map(|_| id)
        };
        local().or_else(|| {
            let r = self.resolve(system, m, path, true)?;
            let name = r.item.filter(|_| r.found)?;
            let mut p = r.module;
            p.push(name);
            let id = NodeId::within(Level::Component, r.system, p);
            self.model.node(&id).map(|_| id)
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
