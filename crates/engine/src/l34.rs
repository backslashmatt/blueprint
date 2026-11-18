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
//! Ids follow DR 0008: an L3 id is its module's path plus the item name, and
//! an L4 id its component's path plus the function's name, with `<Trait>::`
//! for trait methods and `~2`, `~3`… for any clash left.

use std::collections::{BTreeMap, HashMap};

use blueprint_lang_rust::Rust;
use blueprint_model::lang::{FileFacts, Item, ItemKind, LanguageAnalyser};
use blueprint_model::{
    Code, Confidence, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey, Visibility,
};
use rayon::prelude::*;

use crate::l1::{Diagnostic, Read};
use crate::l2::Placement;

/// The reserved L3 segment for a module's free functions.
pub const FUNCTIONS: &str = "(functions)";

/// Adds L3 and L4 nodes for every placed file to `model`, which must already
/// hold the L1–L2 model those placements refer to.
pub fn analyse(
    placed: &BTreeMap<RepoPath, Placement>,
    read: Read<'_>,
    model: &mut Model,
) -> Vec<Diagnostic> {
    let rust = Rust;
    // Read sequentially (the reader need not be thread-safe), parse in
    // parallel, then build nodes in path order so ids are deterministic.
    let mut sources = Vec::new();
    let mut plain = Vec::new();
    for (path, place) in placed {
        match &place.module {
            Some(module) if rust.handles(path) => match read(path) {
                Some(bytes) => sources.push((path, place, module, bytes)),
                None => plain.push((path, place)),
            },
            _ => plain.push((path, place)),
        }
    }
    let parsed: Vec<_> = sources
        .into_par_iter()
        .map(|(path, place, module, bytes)| (path, place, module, rust.analyse(path, &bytes)))
        .collect();

    let mut b = Builder {
        model,
        diags: Vec::new(),
        types: HashMap::new(),
    };
    // Types first, so that a method finds its type wherever the type is.
    for (path, place, module, facts) in &parsed {
        for item in &facts.items {
            if matches!(item.kind, ItemKind::Type { .. }) {
                b.add_type(path, &place.system, module, item, facts);
            }
        }
    }
    for (path, place, module, facts) in &parsed {
        for item in &facts.items {
            if !matches!(item.kind, ItemKind::Type { .. }) {
                b.add_function(path, &place.system, module, item, facts);
            }
        }
    }
    for (path, place) in plain {
        b.add_file(path, place);
    }
    b.diags.sort();
    b.diags
}

struct Builder<'a> {
    model: &'a mut Model,
    diags: Vec<Diagnostic>,
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
    ) {
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
        if let Some(id) = added {
            self.types
                .entry((system.clone(), item.name.clone()))
                .or_default()
                .push(id);
        }
    }

    fn add_function(
        &mut self,
        path: &RepoPath,
        system: &SystemKey,
        file_module: &[String],
        item: &Item,
        facts: &FileFacts,
    ) {
        let module = [file_module, item.module.as_slice()].concat();
        let (module_id, conf) = self.module(system, &module, path);
        let (parent, segment, conf) = match &item.kind {
            ItemKind::Method { self_type, r#trait } => {
                let segment = match r#trait {
                    Some(t) if t != self_type => format!("<{t}>::{}", item.name),
                    _ => item.name.clone(),
                };
                match self.type_for(system, &module_id, self_type) {
                    Some(ty) => (ty, segment, conf),
                    None => {
                        let group =
                            self.group(&module_id, &format!("impl {self_type}"), path, conf);
                        (group, segment, Confidence::Inferred.min(conf))
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
        });
    }

    /// The L3 node of the type `name` for a method in `module`: the one in
    /// that module if there is one, else the only one of that name in the
    /// system. `None` if there is none, or several and none in `module`.
    fn type_for(&self, system: &SystemKey, module: &NodeId, name: &str) -> Option<NodeId> {
        let found = self.types.get(&(system.clone(), name.to_owned()))?;
        found
            .iter()
            .find(|t| {
                t.path().len() == module.path().len() + 1 && t.path().starts_with(module.path())
            })
            .or_else(|| (found.len() == 1).then(|| &found[0]))
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
