//! L3–L4 analysis (DR 0008): types, groups of functions, and functions, from
//! each language analyser's per-file facts, placed under the L2 modules.
//!
//! - **Analysed files** (Rust files the module tree reached): each type is an
//!   L3 component; free functions sit under their module's `(functions)`
//!   component; methods sit under their type when it is in the same crate,
//!   otherwise under an `impl <Type>` component in the impl's module.
//! - **Other files** (other languages, and Rust outside the module tree):
//!   one L3 component per file, `unknown`, with no L4 (§3.4).
//!
//! Edges between them come from the analysers' references ([`edges`]).
//!
//! Ids follow DR 0008: an L3 id is its module's path plus the item name, and
//! an L4 id its component's path plus the function's name, with `<Trait>::`
//! for trait methods and `~2`, `~3`… for any clash left.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use blueprint_lang_rust::Rust;
use blueprint_model::lang::{FileFacts, Item, ItemKind, LanguageAnalyser};
use blueprint_model::{
    Code, Confidence, EdgeKey, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey,
    Visibility,
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
    run(placed, read, cached, parsed, model).0
}

/// What the L3–L4 pass knows about one analysed file, kept by a resident
/// engine so that the file can be updated alone ([`update_body`]).
pub(crate) struct FileState {
    system: SystemKey,
    /// The module the file is.
    module: Vec<String>,
    facts: FileFacts,
    /// The node each item became, by its index in the facts.
    nodes: Vec<Option<NodeId>>,
    /// The file's `calls` edges.
    calls: BTreeSet<EdgeKey>,
}

/// Everything [`update_body`] needs: each analysed file's state, and the
/// context references were resolved in.
pub(crate) struct State {
    files: HashMap<RepoPath, FileState>,
    context: edges::Context,
}

impl State {
    /// The facts of an analysed file.
    pub(crate) fn facts(&self, path: &RepoPath) -> Option<&FileFacts> {
        self.files.get(path).map(|f| &f.facts)
    }
}

/// [`analyse_cached`], also returning the state a resident engine keeps.
pub(crate) fn run(
    placed: &BTreeMap<RepoPath, Placement>,
    read: Read<'_>,
    cached: Cached<'_>,
    parsed: &mut dyn FnMut(&RepoPath, &FileFacts),
    model: &mut Model,
) -> (Vec<Diagnostic>, State) {
    let rust = Rust;
    let mut wanted = Vec::new();
    let mut plain = Vec::new();
    for (path, place) in placed {
        match &place.module {
            Some(module) if rust.handles(path) => wanted.push((path, place, module)),
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
        .map(|(path, place, module, bytes)| (path, place, module, rust.analyse(path, &bytes)))
        .collect();
    for (path, _, _, facts) in &fresh {
        parsed(path, facts);
    }
    all.extend(fresh);
    all.sort_by(|a, b| a.0.cmp(b.0));
    plain.sort_by(|a, b| a.0.cmp(b.0));
    let parsed = all;

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
    let mut nodes: Vec<Vec<Option<NodeId>>> = parsed
        .iter()
        .map(|(_, _, _, facts)| vec![None; facts.items.len()])
        .collect();
    // Types first, so that a method finds its type wherever the type is.
    for (f, (path, place, module, facts)) in parsed.iter().enumerate() {
        for (i, item) in facts.items.iter().enumerate() {
            if matches!(item.kind, ItemKind::Type { .. }) {
                nodes[f][i] = b.add_type(path, &place.system, module, item, facts);
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
                nodes[f][i] = Some(id.clone());
                functions.insert((f, i), id);
            }
        }
    }
    for (path, place) in plain {
        b.add_file(path, place);
    }
    let (context, calls) = edges::add(&mut b, &parsed, &functions, read);
    b.diags.sort();
    let files = parsed
        .into_iter()
        .zip(nodes)
        .zip(calls)
        .map(|(((path, place, module, facts), nodes), calls)| {
            let state = FileState {
                system: place.system.clone(),
                module: module.clone(),
                facts,
                nodes,
                calls,
            };
            (path.clone(), state)
        })
        .collect();
    (b.diags, State { files, context })
}

/// Why [`update_body`] could not update a file alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotBody {
    /// The file was not analysed: not Rust, or no module tree reached it.
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
        &file.system,
        &file.module,
        &facts,
        |i| file.nodes.get(i).and_then(Option::as_ref),
    );
    for gone in file.calls.difference(&calls) {
        edges::remove_rolled(model, gone);
    }
    for new in calls.difference(&file.calls) {
        edges::add_rolled(model, new.clone(), Confidence::Inferred);
    }
    file.calls = calls;
    file.facts = facts;
    Ok(())
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
                match self.type_in(&module_id, system, self_type) {
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

/// An item is as sure as its module, and at most `inferred` if its file had
/// to be recovered from syntax errors.
fn item_confidence(module: Confidence, facts: &FileFacts) -> Confidence {
    if facts.clean {
        module
    } else {
        module.min(Confidence::Inferred)
    }
}
