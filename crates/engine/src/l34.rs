//! L3–L4 analysis (DR 0008): types, groups of functions, and functions, from
//! each language analyser's per-file facts, placed under the L2 modules.
//!
//! - **Analysed files** (files an L2 pass made modules of: Rust files the
//!   module tree reached, TypeScript and JavaScript files of a package):
//!   each type is an L3 component; free functions sit under their module's `(functions)`
//!   component; methods sit under their type when it is in the same crate,
//!   otherwise under an `impl <Type>` component in the impl's module.
//! - **Other files** (languages with no analyser, and files no L2 pass
//!   reached):
//!   one L3 component per file, `unknown`, with no L4 (§3.4).
//!
//! Edges between them come from the analysers' references ([`edges`]); so
//! far only Rust's references are resolved (#139).
//!
//! Ids follow DR 0008: an L3 id is its module's path plus the item name, and
//! an L4 id its component's path plus the function's name, with `<Trait>::`
//! for trait methods and `~2`, `~3`… for any clash left.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use blueprint_model::lang::{FileFacts, Item, ItemKind, Reference};
use blueprint_model::{
    Code, Confidence, Ecosystem, EdgeKey, EdgeKind, Level, Model, Node, NodeId, NodeKind, RepoPath,
    SystemKey, Visibility,
};
use rayon::prelude::*;

use crate::l1::{Diagnostic, Read};
use crate::l2::Placement;

mod edges;

/// One analysed file: its path, placement, module, and facts.
type Parsed<'a> = (&'a RepoPath, &'a Placement, &'a Vec<String>, FileFacts);

/// The reserved L3 segment for a module's free functions.
pub const FUNCTIONS: &str = "(functions)";

/// Facts already known for a file, such as those the index holds for its
/// blob. `None` means the file must be parsed.
pub type Cached<'a> = &'a (dyn Fn(&RepoPath) -> Option<FileFacts> + Sync);

/// Adds L3 and L4 nodes for every placed file to `model`, which must already
/// hold the L1–L2 model those placements refer to.
pub fn analyse(
    placed: &BTreeMap<RepoPath, Placement>,
    read: Read<'_>,
    model: &mut Model,
) -> Vec<Diagnostic> {
    analyse_cached(placed, read, &|_| None, &mut |_, _| {}, model)
}

/// As [`analyse`], but takes each file's facts from `cached` when it has
/// them, and parses only the rest. `parsed` is called, in path order, with
/// every file that had to be parsed, so that its facts can be stored.
pub fn analyse_cached(
    placed: &BTreeMap<RepoPath, Placement>,
    read: Read<'_>,
    cached: Cached<'_>,
    parsed: &mut dyn FnMut(&RepoPath, &FileFacts),
    model: &mut Model,
) -> Vec<Diagnostic> {
    run(placed, read, cached, parsed, model, false).0
}

/// What the L3–L4 pass knows about one analysed file, kept by a resident
/// engine so that the file can be updated alone ([`update_body`],
/// [`update_items`]).
pub(crate) struct FileState {
    system: SystemKey,
    /// The module the file is.
    module: Vec<String>,
    facts: FileFacts,
    /// The node each item became, by its index in the facts.
    nodes: Vec<Option<NodeId>>,
    /// Every L3–L4 node the file made: its items and their groups.
    owned: Vec<NodeId>,
    /// The file's direct edges.
    edges: edges::Contributions,
}

/// Everything a resident engine needs to update one file: each analysed
/// file's state, the context references were resolved in, and indexes of
/// who gives each edge and who mentions each name.
pub(crate) struct State {
    files: HashMap<RepoPath, FileState>,
    context: edges::Context,
    /// Each system's crate root file.
    roots: HashMap<SystemKey, RepoPath>,
    /// For each direct edge, the files that give it, with their confidence.
    producers: HashMap<EdgeKey, BTreeMap<RepoPath, Confidence>>,
    /// The confidence each L1 edge was declared with, before roll-up.
    l1: HashMap<EdgeKey, Confidence>,
    /// The files whose references or calls mention each name.
    mentions: HashMap<String, BTreeSet<RepoPath>>,
    /// For each crate-root L2 node, how many direct edges name it: it
    /// exists while any does, or while it has children.
    root_refs: HashMap<NodeId, usize>,
    /// The names of C# `partial` types, by system. Which of two same-named
    /// types gets the plain id depends on every file that declares one, so
    /// a file declaring such a name is not updated alone (DR 0013). Only a
    /// rebuild changes the set: a file with a partial type always rebuilds.
    partials: HashSet<(SystemKey, String)>,
}

impl State {
    /// The facts of an analysed file.
    pub(crate) fn facts(&self, path: &RepoPath) -> Option<&FileFacts> {
        self.files.get(path).map(|f| &f.facts)
    }
}

/// [`analyse_cached`]; with `keep`, also returns the state a resident engine
/// keeps. Without it, nothing extra is built.
pub(crate) fn run(
    placed: &BTreeMap<RepoPath, Placement>,
    read: Read<'_>,
    cached: Cached<'_>,
    parsed: &mut dyn FnMut(&RepoPath, &FileFacts),
    model: &mut Model,
    keep: bool,
) -> (Vec<Diagnostic>, Option<State>) {
    let mut wanted = Vec::new();
    let mut plain = Vec::new();
    for (path, place) in placed {
        match (
            &place.module,
            crate::lang::analyser_in(path, place.system.ecosystem),
        ) {
            (Some(module), Some(_)) => wanted.push((path, place, module)),
            _ => plain.push((path, place)),
        }
    }
    // Look up stored facts in parallel (decoding them is the cost), read the
    // rest sequentially (the reader need not be thread-safe), parse those in
    // parallel, then build nodes in path order so ids are deterministic.
    let found: Vec<Option<FileFacts>> = wanted.par_iter().map(|(p, _, _)| cached(p)).collect();
    let mut all: Vec<Parsed<'_>> = Vec::with_capacity(wanted.len());
    let mut sources = Vec::new();
    for ((path, place, module), facts) in wanted.into_iter().zip(found) {
        match facts {
            Some(facts) => all.push((path, place, module, facts)),
            None => match read(path) {
                Some(bytes) => sources.push((path, place, module, bytes)),
                None => plain.push((path, place)),
            },
        }
    }
    let fresh: Vec<Parsed<'_>> = sources
        .into_par_iter()
        .filter_map(|(path, place, module, bytes)| {
            let analyser = crate::lang::analyser_in(path, place.system.ecosystem)?;
            let facts = analyser.analyse(path, &bytes);
            Some((path, place, module, facts))
        })
        .collect();
    for (path, _, _, facts) in &fresh {
        parsed(path, facts);
    }
    all.extend(fresh);
    all.sort_by(|a, b| a.0.cmp(b.0));
    plain.sort_by(|a, b| a.0.cmp(b.0));
    let parsed = all;

    // The confidence the manifests declared each L1 edge with, before
    // roll-up adds evidence.
    let l1: HashMap<EdgeKey, Confidence> = if keep {
        model
            .edges()
            .filter(|e| e.key.from.level() == Level::System)
            .map(|e| (e.key.clone(), e.confidence))
            .collect()
    } else {
        HashMap::new()
    };
    let roots = placed
        .iter()
        .filter(|(_, p)| p.module.as_ref().is_some_and(Vec::is_empty))
        .map(|(path, p)| (p.system.clone(), path.clone()))
        .rev()
        .collect();
    let mut b = Builder {
        model,
        roots,
        diags: Vec::new(),
        types: HashMap::new(),
    };
    // The node each item became, by file and item index.
    let mut nodes: Vec<Vec<Option<NodeId>>> = if keep {
        parsed
            .iter()
            .map(|(_, _, _, facts)| vec![None; facts.items.len()])
            .collect()
    } else {
        Vec::new()
    };
    let mut record = |f: usize, i: usize, id: Option<NodeId>| {
        if let Some(slot) = nodes.get_mut(f).and_then(|n| n.get_mut(i)) {
            *slot = id;
        }
    };
    // Types first, so that a method finds its type wherever the type is.
    for (f, (path, place, module, facts)) in parsed.iter().enumerate() {
        for (i, item) in facts.items.iter().enumerate() {
            if matches!(item.kind, ItemKind::Type { .. }) {
                record(f, i, b.add_type(path, &place.system, module, item, facts));
            }
        }
    }
    // The L4 id each function got, by (file, item) index, for calls.
    let mut functions: HashMap<(usize, usize), NodeId> = HashMap::new();
    for (f, (path, place, module, facts)) in parsed.iter().enumerate() {
        for (i, item) in facts.items.iter().enumerate() {
            if !matches!(item.kind, ItemKind::Type { .. })
                && let Some(id) = b.add_function(path, &place.system, module, item, facts)
            {
                if keep {
                    record(f, i, Some(id.clone()));
                }
                functions.insert((f, i), id);
            }
        }
    }
    for (path, place) in plain {
        b.add_file(path, place);
    }
    let kept = edges::add(&mut b, &parsed, &functions, read, keep);
    b.diags.sort();
    let Some((context, contributions)) = kept else {
        return (b.diags, None);
    };
    let mut state = State {
        files: HashMap::new(),
        context,
        roots: b.roots.clone(),
        producers: HashMap::new(),
        l1,
        mentions: HashMap::new(),
        root_refs: HashMap::new(),
        partials: parsed
            .iter()
            .flat_map(|(_, place, _, facts)| {
                facts.items.iter().filter_map(|i| match &i.kind {
                    ItemKind::Type { keyword } if keyword.starts_with("partial ") => {
                        Some((place.system.clone(), i.name.clone()))
                    }
                    _ => None,
                })
            })
            .collect(),
    };
    for (((path, place, module, facts), nodes), edges) in
        parsed.into_iter().zip(nodes).zip(contributions)
    {
        let owned = owned(b.model, &nodes);
        state.produce(path, &edges);
        state.mention(path, &facts, true);
        let file = FileState {
            system: place.system.clone(),
            module: module.clone(),
            facts,
            nodes,
            owned,
            edges,
        };
        state.files.insert(path.clone(), file);
    }
    (b.diags, Some(state))
}

/// The nodes a file's items made, and the groups they sit in.
fn owned(model: &Model, nodes: &[Option<NodeId>]) -> Vec<NodeId> {
    let mut out: BTreeSet<NodeId> = BTreeSet::new();
    for id in nodes.iter().flatten() {
        out.insert(id.clone());
        if let Some(parent) = model.node(id).and_then(|n| n.parent.as_ref())
            && parent.level() == Level::Component
            && model.node(parent).is_some_and(|p| p.code.is_none())
        {
            out.insert(parent.clone());
        }
    }
    out.into_iter().collect()
}

/// Every name a file's references and calls mention, path segments
/// included: a change to an item of that name elsewhere can change how
/// they resolve.
fn mentioned(facts: &FileFacts) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    for r in &facts.references {
        match r {
            Reference::Use { path, alias, .. } => {
                out.extend(path.iter().map(String::as_str));
                out.extend(alias.as_deref());
            }
            Reference::Impl {
                self_type, r#trait, ..
            } => {
                out.extend(self_type.iter().map(String::as_str));
                out.extend(r#trait.iter().map(String::as_str));
            }
        }
    }
    for item in &facts.items {
        for call in &item.calls {
            out.extend(call.iter().map(String::as_str));
        }
    }
    out
}

/// Whether a file of another crate can resolve a path to an item named in
/// `changed` in the crate root it knows by one of `externs` (a root item,
/// a method of a root type, an item of an inline module). Paths resolve
/// through the file's own `use`s, once, and never through re-exports, so a
/// path can only reach there if it is
/// - `extern::…` with a changed name or a glob anywhere after the crate,
///   in a `use`, an `impl` or a call;
/// - the crate itself, bound by a `use` (`use extern as x;`);
/// - `b::…` with a changed name after `b`, where `b` is a name a `use` of
///   `extern::…` binds (`use extern::S;` then `S::new()`).
fn reaches_root(facts: &FileFacts, externs: &BTreeSet<&str>, changed: &BTreeSet<&str>) -> bool {
    let mut bound: BTreeSet<&str> = BTreeSet::new();
    for r in &facts.references {
        if let Reference::Use { path, alias, .. } = r
            && path.first().is_some_and(|f| externs.contains(f.as_str()))
        {
            let name = alias.as_deref().or_else(|| path.last().map(String::as_str));
            bound.extend(name.filter(|n| *n != "*"));
        }
    }
    let after = |rest: &[String]| {
        rest.iter()
            .any(|s| s == "*" || changed.contains(s.as_str()))
    };
    let hits = |p: &[String], is_use: bool| match p {
        [first] if externs.contains(first.as_str()) => is_use,
        [first, rest @ ..] if externs.contains(first.as_str()) => after(rest),
        [first, rest @ ..] if bound.contains(first.as_str()) => after(rest),
        _ => false,
    };
    facts.references.iter().any(|r| match r {
        Reference::Use { path, .. } => hits(path, true),
        Reference::Impl {
            self_type, r#trait, ..
        } => hits(self_type, false) || hits(r#trait, false),
    }) || facts
        .items
        .iter()
        .flat_map(|i| &i.calls)
        .any(|c| hits(c, false))
}

/// The names a file's items give the resolution index: each L3 node's last
/// segment in its module, and each free function's name in its module.
fn indexed_names(owned: &[NodeId]) -> BTreeSet<(Vec<String>, String)> {
    let mut out = BTreeSet::new();
    for id in owned {
        match (id.level(), id.path()) {
            (Level::Component, [module @ .., name]) => {
                out.insert((module.to_vec(), name.clone()));
            }
            (Level::Code, [module @ .., group, name]) if group == FUNCTIONS => {
                out.insert((module.to_vec(), name.clone()));
            }
            _ => {}
        }
    }
    out
}

impl State {
    /// Records `path` as a source of each edge in `edges`.
    fn produce(&mut self, path: &RepoPath, edges: &edges::Contributions) {
        for (k, c) in edges {
            let by = self.producers.entry(k.clone()).or_default();
            if by.is_empty() {
                for end in [&k.from, &k.to] {
                    if end.level() == Level::Module && end.path().is_empty() {
                        *self.root_refs.entry(end.clone()).or_default() += 1;
                    }
                }
            }
            by.insert(path.clone(), *c);
        }
    }

    /// Records (`add`) or forgets the names `facts` mention for `path`.
    fn mention(&mut self, path: &RepoPath, facts: &FileFacts, add: bool) {
        for name in mentioned(facts) {
            if add {
                self.mentions
                    .entry(name.to_owned())
                    .or_default()
                    .insert(path.clone());
            } else if let Some(files) = self.mentions.get_mut(name) {
                files.remove(path);
            }
        }
    }
}

/// Why [`update_body`] could not update a file alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotBody {
    /// The file was not analysed: no analyser reads it, or no L2 pass reached it.
    NotAnalysed,
    /// Its items, references or parse state changed, so other files' nodes
    /// or edges can change too.
    Structure,
}

/// Updates one analysed file whose new `facts` differ from its old ones only
/// within items: the same items (kind, name and module, in order), the same
/// references, and the same parse state. Its nodes get the new spans,
/// hashes and visibility, and its `calls` edges are resolved again, rolled
/// up. Nothing else in the model can change, because what other files
/// resolve against is unchanged (DR 0008 decision 5).
pub(crate) fn update_body(
    model: &mut Model,
    state: &mut State,
    path: &RepoPath,
    facts: FileFacts,
) -> Result<(), NotBody> {
    let file = state.files.get_mut(path).ok_or(NotBody::NotAnalysed)?;
    // A partial type's node is shared with its other parts' files.
    if has_partial(&file.facts) || has_partial(&facts) {
        return Err(NotBody::Structure);
    }
    let same_items = file.facts.items.len() == facts.items.len()
        && file
            .facts
            .items
            .iter()
            .zip(&facts.items)
            .all(|(a, b)| a.kind == b.kind && a.name == b.name && a.module == b.module);
    if !same_items || file.facts.references != facts.references || file.facts.clean != facts.clean {
        return Err(NotBody::Structure);
    }
    for (item, id) in facts.items.iter().zip(&file.nodes) {
        let Some(mut node) = id.as_ref().and_then(|id| model.node(id)).cloned() else {
            continue;
        };
        node.code = Some(code(item));
        node.visibility = item.visibility;
        let _ = model.replace_node(node);
    }
    let calls = edges::calls_of(
        model,
        &state.context,
        path,
        &file.system,
        &file.module,
        &facts,
        |i| file.nodes.get(i).and_then(Option::as_ref),
    );
    let old: BTreeSet<EdgeKey> = file
        .edges
        .keys()
        .filter(|k| k.kind == EdgeKind::Calls)
        .cloned()
        .collect();
    // A call is only ever given by its caller's file.
    for gone in old.difference(&calls) {
        file.edges.remove(gone);
        state.producers.remove(gone);
        edges::remove_rolled(model, gone, &state.l1);
    }
    for new in calls.difference(&old) {
        file.edges.insert(new.clone(), Confidence::Inferred);
        state
            .producers
            .entry(new.clone())
            .or_default()
            .insert(path.clone(), Confidence::Inferred);
        edges::add_rolled(model, new.clone(), Confidence::Inferred);
    }
    file.facts = facts;
    Ok(())
}

/// Updates one analysed file whose items or references changed, as DR
/// 0008 decision 5 describes: its nodes are made again, the resolution
/// index and its imports updated, and its own references re-resolved,
/// with those of every other file that mentions a name it added or
/// removed. Edges change by difference, rolled up. A crate's root file,
/// or a file whose module has no L2 node, is not updated here: nodes and
/// groups there can be shared with other files.
pub(crate) fn update_items(
    model: &mut Model,
    state: &mut State,
    path: &RepoPath,
    facts: FileFacts,
) -> Result<(), NotBody> {
    let file = state.files.get(path).ok_or(NotBody::NotAnalysed)?;
    if has_partial(&file.facts) || has_partial(&facts) {
        return Err(NotBody::Structure);
    }
    // A type named as a partial one elsewhere: the ids of both depend on
    // every file that declares the name.
    let shares_partial = |f: &FileFacts| {
        f.items.iter().any(|i| {
            matches!(i.kind, ItemKind::Type { .. })
                && state
                    .partials
                    .contains(&(file.system.clone(), i.name.clone()))
        })
    };
    if shares_partial(&file.facts) || shares_partial(&facts) {
        return Err(NotBody::Structure);
    }
    // Go: a package's files share its `(functions)` group, and its methods
    // attach to types in other files, so an item change in one file can
    // move nodes another file made (DR 0015).
    if file.system.ecosystem == Ecosystem::Go {
        return Err(NotBody::Structure);
    }
    let (system, module) = (file.system.clone(), file.module.clone());
    let module_id = NodeId::within(Level::Module, system.clone(), module.clone());
    // A crate root's own L2 node exists only while something needs it
    // (its items, or an edge), so it may be missing; the builder makes it
    // when an item needs it, and step 4 drops it when nothing does.
    let root = module.is_empty();
    if !root && model.node(&module_id).is_none() {
        return Err(NotBody::Structure);
    }
    // Inline modules must have their own L2 nodes too, or items would sit
    // in a module another file shares.
    for item in &facts.items {
        let m = [module.as_slice(), item.module.as_slice()].concat();
        if !m.is_empty()
            && model
                .node(&NodeId::within(Level::Module, system.clone(), m))
                .is_none()
        {
            return Err(NotBody::Structure);
        }
    }
    // An item is what its name resolves to: its module, its name and its
    // kind, which for a method holds its type and trait. A name changes when
    // any of those does, so a function that becomes a type or a method,
    // or a method that moves to another type or into a trait impl, counts.
    let item_names = |f: &FileFacts| -> BTreeSet<(Vec<String>, String, ItemKind)> {
        f.items
            .iter()
            .map(|i| (i.module.clone(), i.name.clone(), i.kind.clone()))
            .collect()
    };
    let (old_names, new_names) = (item_names(&file.facts), item_names(&facts));
    let changed: BTreeSet<&str> = old_names
        .symmetric_difference(&new_names)
        .map(|(_, n, _)| n.as_str())
        .collect();
    // Another file's reference can only resolve into this file's module by
    // a path that names every segment of the module below the deepest
    // module the two files share: from the crate root, another crate,
    // `super`, a child module or a `use` binding alike. Of the files that
    // mention a changed name, only those that also mention all of those
    // segments can change.
    let mentions = &state.mentions;
    let mentions_all = |g: &RepoPath, segs: &[String]| {
        segs.iter().all(|s| {
            mentions
                .get(s.as_str())
                .is_some_and(|files| files.contains(g))
        })
    };
    // A TypeScript or JavaScript file reaches another's module by a
    // specifier, not by its segments, so there any file of a package that
    // mentions a changed name can change; the two languages' references
    // never resolve into each other.
    let package = |s: &SystemKey| matches!(s.ecosystem, Ecosystem::Npm | Ecosystem::Nx);
    let extern_names = state.context.extern_names(&system);
    let mut touched: BTreeSet<RepoPath> = BTreeSet::from([path.clone()]);
    // A method that comes or goes is reached through its type too: a
    // Python `C()` calls `C.__init__`, and a call through `self` looks in
    // the bases.
    let owners: BTreeSet<&str> = old_names
        .symmetric_difference(&new_names)
        .filter_map(|(_, _, k)| match k {
            ItemKind::Method { self_type, .. } => Some(self_type.as_str()),
            _ => None,
        })
        .collect();
    // A nested type is `Outer.Inner`, mentioned as `Outer` and `Inner`.
    let lookups: BTreeSet<&str> = changed
        .iter()
        .chain(&owners)
        .flat_map(|n| std::iter::once(*n).chain(n.split('.')))
        .collect();
    for name in &lookups {
        for g in mentions.get(*name).into_iter().flatten() {
            let Some(f) = state.files.get(g) else {
                continue;
            };
            if package(&f.system) != package(&system) {
                continue;
            }
            // C#: a name reaches another file through its namespace's
            // `using`s or a qualified name, in its project or one that
            // references it, so any C# file that mentions it can change.
            // Python: through an import, from any Python project (#157).
            let by_name =
                |s: &SystemKey| matches!(s.ecosystem, Ecosystem::Dotnet | Ecosystem::Python);
            if by_name(&system) || by_name(&f.system) {
                if system.ecosystem == f.system.ecosystem {
                    touched.insert(g.clone());
                }
                continue;
            }
            if package(&system) {
                touched.insert(g.clone());
                continue;
            }
            let shared = if f.system == system {
                module
                    .iter()
                    .zip(&f.module)
                    .take_while(|(a, b)| a == b)
                    .count()
            } else {
                // Another crate reaches this one only by an extern name,
                // and a crate root's items only right after it.
                if !extern_names
                    .iter()
                    .any(|n| mentions_all(g, &[(*n).to_owned()]))
                {
                    continue;
                }
                if root && !reaches_root(&f.facts, &extern_names, &changed) {
                    continue;
                }
                0
            };
            if mentions_all(g, &module[shared..]) {
                touched.insert(g.clone());
            }
        }
    }

    // 1. The touched files' edges go, rolled up.
    let mut roots_to_check: BTreeSet<NodeId> = BTreeSet::new();
    if root {
        roots_to_check.insert(module_id.clone());
    }
    for p in &touched {
        let Some(f) = state.files.get_mut(p) else {
            continue;
        };
        let edges = std::mem::take(&mut f.edges);
        for k in edges.keys() {
            state.unproduce(model, p, k, &mut roots_to_check);
        }
    }

    // 2. The file's nodes are made again.
    let Some(file) = state.files.get_mut(path) else {
        return Err(NotBody::NotAnalysed);
    };
    let old_owned = std::mem::take(&mut file.owned);
    for id in old_owned.iter().rev() {
        model.detach_node(id);
    }
    let mut b = Builder {
        model,
        roots: state.roots.clone(),
        diags: Vec::new(),
        types: HashMap::new(),
    };
    let mut nodes = vec![None; facts.items.len()];
    for (i, item) in facts.items.iter().enumerate() {
        if matches!(item.kind, ItemKind::Type { .. }) {
            nodes[i] = b.add_type(path, &system, &module, item, &facts);
        }
    }
    for (i, item) in facts.items.iter().enumerate() {
        if !matches!(item.kind, ItemKind::Type { .. }) {
            nodes[i] = b.add_function(path, &system, &module, item, &facts);
        }
    }
    let model = b.model;
    let new_owned = owned(model, &nodes);
    state.context.rename(
        &system,
        &indexed_names(&old_owned),
        &indexed_names(&new_owned),
    );
    state.context.rebind(&system, &module, &file.facts, &facts);
    state.context.rekind(&system, &module, &file.facts, &facts);
    let old_facts = std::mem::replace(&mut file.facts, facts);
    file.nodes = nodes;
    file.owned = new_owned;
    state.mention(path, &old_facts, false);
    let new_facts = state.files[path].facts.clone();
    state.mention(path, &new_facts, true);

    // 3. The touched files' edges are found again.
    for p in &touched {
        let Some(f) = state.files.get(p) else {
            continue;
        };
        let found = edges::edges_of(
            model,
            &state.context,
            p,
            &f.system,
            &f.module,
            &f.facts,
            |i| f.nodes.get(i).and_then(Option::as_ref),
        );
        for (k, c) in &found {
            // A crate root's L2 node exists only once something needs it.
            for end in [&k.from, &k.to] {
                if end.level() == Level::Module
                    && end.path().is_empty()
                    && model.node(end).is_none()
                {
                    let mut b = Builder {
                        model,
                        roots: state.roots.clone(),
                        diags: Vec::new(),
                        types: HashMap::new(),
                    };
                    b.module(end.system_key(), &[], p);
                }
            }
            state.produce_one(model, p, k, *c);
        }
        if let Some(f) = state.files.get_mut(p) {
            f.edges = found;
        }
    }

    // 4. A crate root's L2 node that nothing needs any more goes.
    for root in roots_to_check {
        let needed = state.root_refs.get(&root).copied().unwrap_or(0) > 0
            || model.children(&root).next().is_some();
        if !needed {
            model.detach_node(&root);
        }
    }
    Ok(())
}

impl State {
    /// `path` no longer gives the edge `k`. If no file gives it any more it
    /// goes, rolled up; otherwise it keeps the surest of what is left.
    fn unproduce(
        &mut self,
        model: &mut Model,
        path: &RepoPath,
        k: &EdgeKey,
        roots: &mut BTreeSet<NodeId>,
    ) {
        let Some(by) = self.producers.get_mut(k) else {
            return;
        };
        let before = by.values().copied().max();
        by.remove(path);
        let after = by.values().copied().max();
        if after.is_none() {
            self.producers.remove(k);
            for end in [&k.from, &k.to] {
                if end.level() == Level::Module && end.path().is_empty() {
                    if let Some(n) = self.root_refs.get_mut(end) {
                        *n -= 1;
                        if *n == 0 {
                            self.root_refs.remove(end);
                        }
                    }
                    roots.insert(end.clone());
                }
            }
            edges::remove_rolled(model, k, &self.l1);
        } else if after != before {
            edges::remove_rolled(model, k, &self.l1);
            if let Some(c) = after {
                edges::add_rolled(model, k.clone(), c);
            }
        }
    }

    /// `path` gives the edge `k` with confidence `c`: it is added, rolled
    /// up, or made surer.
    fn produce_one(&mut self, model: &mut Model, path: &RepoPath, k: &EdgeKey, c: Confidence) {
        let before = self
            .producers
            .get(k)
            .and_then(|by| by.values().copied().max());
        self.produce(path, &BTreeMap::from([(k.clone(), c)]));
        if before.is_none() {
            edges::add_rolled(model, k.clone(), c);
        } else if before.is_some_and(|b| c > b) {
            edges::remove_rolled(model, k, &self.l1);
            edges::add_rolled(model, k.clone(), c);
        }
    }
}

struct Builder<'a> {
    model: &'a mut Model,
    diags: Vec<Diagnostic>,
    /// Each system's crate root file: the file placed at module `[]`.
    roots: HashMap<SystemKey, RepoPath>,
    /// Every type added, by system and name: the L3 ids of that name.
    types: HashMap<(SystemKey, String), Vec<NodeId>>,
}

impl Builder<'_> {
    fn add(&mut self, node: Node) -> Option<NodeId> {
        let id = node.id.clone();
        let source = node.source.clone();
        match self.model.add_node(node) {
            Ok(()) => Some(id),
            Err(e) => {
                self.diags.push(Diagnostic {
                    path: source,
                    message: e.to_string(),
                });
                None
            }
        }
    }

    /// The L2 node for `module` in `system`: the module itself, or its
    /// nearest existing ancestor (with `inferred` confidence for what sits
    /// in it), or the crate root, which is added when first needed.
    fn module(
        &mut self,
        system: &SystemKey,
        module: &[String],
        source: &RepoPath,
    ) -> (NodeId, Confidence) {
        for n in (1..=module.len()).rev() {
            let id = NodeId::within(Level::Module, system.clone(), module[..n].to_vec());
            if let Some(node) = self.model.node(&id) {
                let exact = n == module.len();
                let conf = if exact {
                    node.confidence
                } else {
                    Confidence::Inferred
                };
                return (id, conf);
            }
        }
        let root = NodeId::within(Level::Module, system.clone(), Vec::new());
        if self.model.node(&root).is_none() {
            // Named and sourced like the crate: the root file is the one
            // placed at the top of the module tree.
            let source = self.roots.get(system).unwrap_or(source).clone();
            let parent = NodeId::system(system.clone());
            let name = self
                .model
                .node(&parent)
                .map_or_else(|| "crate".to_owned(), |n| n.name.clone());
            self.add(Node {
                id: root.clone(),
                kind: NodeKind::Module,
                name,
                source: source.clone(),
                parent: Some(parent),
                confidence: Confidence::Exact,
                visibility: Some(Visibility::Public),
                code: None,
            });
        }
        let conf = if module.is_empty() {
            Confidence::Exact
        } else {
            Confidence::Inferred
        };
        (root, conf)
    }

    /// The first free id under `parent` for `segment`: the segment itself,
    /// then `segment~2`, `segment~3`…
    fn free_id(&self, level: Level, parent: &NodeId, segment: &str) -> NodeId {
        let at = |s: String| {
            let mut path = parent.path().to_vec();
            path.push(s);
            NodeId::within(level, parent.system_key().clone(), path)
        };
        let mut id = at(segment.to_owned());
        let mut n = 2;
        while self.model.node(&id).is_some() {
            id = at(format!("{segment}~{n}"));
            n += 1;
        }
        id
    }

    fn add_type(
        &mut self,
        path: &RepoPath,
        system: &SystemKey,
        file_module: &[String],
        item: &Item,
        facts: &FileFacts,
    ) -> Option<NodeId> {
        let module = [file_module, item.module.as_slice()].concat();
        let (parent, conf) = self.module(system, &module, path);
        // A C# `partial` type's parts are one node (DR 0013): a later part
        // joins the first part's node, in path order, its hashes mixed in.
        let partial =
            matches!(&item.kind, ItemKind::Type { keyword } if keyword.starts_with("partial "));
        let partial_key = (system.clone(), format!("partial {}", item.name));
        if partial
            && let Some(first) = self.types.get(&partial_key).and_then(|ids| {
                ids.iter().find(|t| {
                    t.path().len() == parent.path().len() + 1 && t.path().starts_with(parent.path())
                })
            })
            && let Some(mut node) = self.model.node(first).cloned()
            && let Some(code) = node.code.as_mut()
        {
            let join = |a: u64, b: u64| (a ^ b.rotate_left(17)).wrapping_mul(0x0100_0000_01b3);
            code.signature_hash = join(code.signature_hash, item.signature_hash);
            code.body_hash = match (code.body_hash, item.body_hash) {
                (Some(a), Some(b)) => Some(join(a, b)),
                (a, b) => a.or(b),
            };
            node.visibility = node.visibility.max(item.visibility);
            let id = node.id.clone();
            let _ = self.model.replace_node(node);
            return Some(id);
        }
        let id = self.free_id(Level::Component, &parent, &item.name);
        let added = self.add(Node {
            id,
            kind: NodeKind::Component,
            name: item.name.clone(),
            source: path.clone(),
            parent: Some(parent),
            confidence: item_confidence(conf, facts),
            visibility: item.visibility,
            code: Some(code(item)),
        });
        if let Some(id) = &added {
            self.types
                .entry((system.clone(), item.name.clone()))
                .or_default()
                .push(id.clone());
            if partial {
                self.types.entry(partial_key).or_default().push(id.clone());
            }
        }
        added
    }

    fn add_function(
        &mut self,
        path: &RepoPath,
        system: &SystemKey,
        file_module: &[String],
        item: &Item,
        facts: &FileFacts,
    ) -> Option<NodeId> {
        let module = [file_module, item.module.as_slice()].concat();
        let (module_id, conf) = self.module(system, &module, path);
        let (parent, segment, conf) = match &item.kind {
            ItemKind::Method { self_type, r#trait } => {
                let segment = match r#trait {
                    Some(t) if t != self_type => format!("<{t}>::{}", item.name),
                    _ => item.name.clone(),
                };
                // Only a type in the impl's own module is certain. Anything
                // else goes to an `impl <Type>` group in the impl's module,
                // so an unrelated type of the same name elsewhere can never
                // move these methods (the diff compares by id).
                // A member of a partial type this file declares belongs to
                // the partial type's node, not to another type of the name.
                let own_partial = facts.items.iter().any(|t| {
                    t.name == *self_type
                        && t.module == item.module
                        && matches!(&t.kind, ItemKind::Type { keyword } if keyword.starts_with("partial "))
                });
                let partial = own_partial
                    .then(|| {
                        self.types
                            .get(&(system.clone(), format!("partial {self_type}")))
                    })
                    .flatten()
                    .and_then(|ids| {
                        ids.iter()
                            .find(|t| {
                                t.path().len() == module_id.path().len() + 1
                                    && t.path().starts_with(module_id.path())
                            })
                            .cloned()
                    });
                match partial.or_else(|| self.type_in(&module_id, system, self_type)) {
                    Some(ty) => (ty, segment, conf),
                    None => {
                        let conf = Confidence::Inferred.min(conf);
                        let group =
                            self.group(&module_id, &format!("impl {self_type}"), path, conf);
                        (group, segment, conf)
                    }
                }
            }
            _ => (
                self.group(&module_id, FUNCTIONS, path, conf),
                item.name.clone(),
                conf,
            ),
        };
        let id = self.free_id(Level::Code, &parent, &segment);
        self.add(Node {
            id,
            kind: NodeKind::Function,
            name: item.name.clone(),
            source: path.clone(),
            parent: Some(parent),
            confidence: item_confidence(conf, facts),
            visibility: item.visibility,
            code: Some(code(item)),
        })
    }

    /// The L3 node of the type `name` declared directly in `module`.
    fn type_in(&self, module: &NodeId, system: &SystemKey, name: &str) -> Option<NodeId> {
        self.types
            .get(&(system.clone(), name.to_owned()))?
            .iter()
            .find(|t| {
                t.path().len() == module.path().len() + 1 && t.path().starts_with(module.path())
            })
            .cloned()
    }

    /// A synthetic L3 component under `module`, added when first needed.
    fn group(
        &mut self,
        module: &NodeId,
        name: &str,
        source: &RepoPath,
        conf: Confidence,
    ) -> NodeId {
        let mut path = module.path().to_vec();
        path.push(name.to_owned());
        let id = NodeId::within(Level::Component, module.system_key().clone(), path);
        if self.model.node(&id).is_none() {
            self.add(Node {
                id: id.clone(),
                kind: NodeKind::Component,
                name: name.to_owned(),
                source: source.clone(),
                parent: Some(module.clone()),
                confidence: conf,
                visibility: None,
                code: None,
            });
        }
        id
    }

    /// One `unknown` L3 component for a file no analyser read, under the L2
    /// directory node that holds it (or `./` for the system's own directory).
    fn add_file(&mut self, path: &RepoPath, place: &Placement) {
        let base = place.system.dir.segments().len();
        let segs = path.segments();
        let Some(rel) = segs.get(base..).filter(|r| !r.is_empty()) else {
            return;
        };
        let dir_seg = if rel.len() > 1 {
            format!("{}/", rel[0])
        } else {
            "./".to_owned()
        };
        let dir = NodeId::within(Level::Module, place.system.clone(), vec![dir_seg.clone()]);
        if self.model.node(&dir).is_none() {
            // Only `./` can be missing: L2 adds the other directories.
            self.add(Node {
                id: dir.clone(),
                kind: NodeKind::Directory,
                name: dir_seg,
                source: place.system.dir.clone(),
                parent: Some(NodeId::system(place.system.clone())),
                confidence: Confidence::Unknown,
                visibility: None,
                code: None,
            });
        }
        let id = NodeId::within(
            Level::Component,
            place.system.clone(),
            vec![dir.path()[0].clone(), rel.join("/")],
        );
        let name = rel.last().cloned().unwrap_or_default();
        self.add(Node {
            id,
            kind: NodeKind::Component,
            name,
            source: path.clone(),
            parent: Some(dir),
            confidence: Confidence::Unknown,
            visibility: None,
            code: None,
        });
    }
}

fn code(item: &Item) -> Code {
    Code {
        lines: item.lines,
        body_line: item.body_line,
        signature_hash: item.signature_hash,
        body_hash: item.body_hash,
    }
}

/// Whether a file declares a C# `partial` type, whose node its other
/// parts' files share (DR 0013): such a file is not updated alone.
fn has_partial(facts: &FileFacts) -> bool {
    facts
        .items
        .iter()
        .any(|i| matches!(&i.kind, ItemKind::Type { keyword } if keyword.starts_with("partial ")))
}

/// An item is as sure as its module, and at most `inferred` if its file had
/// to be recovered from syntax errors.
fn item_confidence(module: Confidence, facts: &FileFacts) -> Confidence {
    if facts.clean {
        module
    } else {
        module.min(Confidence::Inferred)
    }
}
