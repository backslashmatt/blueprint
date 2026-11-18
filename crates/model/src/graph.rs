//! Nodes, edges and the model that holds them (REQUIREMENTS §3.1–3.2).
//!
//! A [`NodeId`] is derived from *where* a node is: its ecosystem, the
//! directory of the manifest that defines its system, and its path inside
//! that system. It never depends on content, and segments are escaped, so two
//! different locations can never share an id. The [`Model`] enforces the
//! hierarchy on every insertion *and* when deserialised, and every collection
//! is ordered, so the JSON form is deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Confidence, Level};

/// The build system or language ecosystem an L1 system comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ecosystem {
    /// `Cargo.toml`.
    Cargo,
    /// `package.json`.
    Npm,
    /// `go.mod`.
    Go,
    /// `*.csproj`.
    Dotnet,
    /// Nx `project.json`.
    Nx,
    /// Bazel `BUILD`.
    Bazel,
    /// Files owned by no manifest.
    Unowned,
}

impl Ecosystem {
    /// Every ecosystem.
    const ALL: [Ecosystem; 7] = [
        Ecosystem::Cargo,
        Ecosystem::Npm,
        Ecosystem::Go,
        Ecosystem::Dotnet,
        Ecosystem::Nx,
        Ecosystem::Bazel,
        Ecosystem::Unowned,
    ];

    /// The ecosystem whose [`Ecosystem::tag`] is `tag`.
    pub(crate) fn from_tag(tag: &str) -> Option<Ecosystem> {
        Self::ALL.into_iter().find(|e| e.tag() == tag)
    }

    /// The short name ids use for it.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Ecosystem::Cargo => "cargo",
            Ecosystem::Npm => "npm",
            Ecosystem::Go => "go",
            Ecosystem::Dotnet => "dotnet",
            Ecosystem::Nx => "nx",
            Ecosystem::Bazel => "bazel",
            Ecosystem::Unowned => "unowned",
        }
    }
}

/// A normalised, repository-relative path: `/`-separated, no empty, `.` or
/// `..` segments, no backslashes. The repository root is the empty path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoPath(pub(crate) Vec<String>);

/// Why a string is not a valid [`RepoPath`] or [`NodeId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidPath(pub String);

impl fmt::Display for InvalidPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "not a normalised repository path or node id: {:?}",
            self.0
        )
    }
}

impl std::error::Error for InvalidPath {}

impl RepoPath {
    /// The repository root.
    pub fn root() -> RepoPath {
        RepoPath(Vec::new())
    }

    /// Parses a normalised path. Callers convert platform paths first.
    pub fn parse(s: &str) -> Result<RepoPath, InvalidPath> {
        if s.is_empty() {
            return Ok(RepoPath::root());
        }
        let segments: Vec<String> = s.split('/').map(str::to_owned).collect();
        let bad = |seg: &String| seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\');
        if segments.iter().any(bad) {
            return Err(InvalidPath(s.to_owned()));
        }
        Ok(RepoPath(segments))
    }

    /// The path's segments.
    pub fn segments(&self) -> &[String] {
        &self.0
    }

    /// True when `self` is `other` or lies inside it.
    pub fn starts_with(&self, other: &RepoPath) -> bool {
        self.0.starts_with(&other.0)
    }
}

impl TryFrom<String> for RepoPath {
    type Error = InvalidPath;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        RepoPath::parse(&s)
    }
}

impl From<RepoPath> for String {
    fn from(p: RepoPath) -> String {
        p.0.join("/")
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("/"))
    }
}

/// Identifies one L1 system: its ecosystem and the directory of its manifest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemKey {
    /// The ecosystem.
    pub ecosystem: Ecosystem,
    /// Directory containing the manifest; the root for unowned files.
    pub dir: RepoPath,
}

/// Stable identity of a node. Text form (escaped so it is unambiguous):
/// `L<level>:<ecosystem>:<manifest dir>#<path within system>`.
///
/// An id is shared, not copied: cloning it is a reference count, because a
/// model holds each id many times (as a key, a parent, edge ends, evidence)
/// and copying them dominated loading and building models (DR 0009).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NodeId(std::sync::Arc<IdParts>);

/// What a [`NodeId`] is made of, in the order ids sort by.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct IdParts {
    level: Level,
    system: SystemKey,
    path: Vec<String>,
}

/// Escapes the characters the id syntax uses, so segments cannot run together.
fn escape(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for c in seg.chars() {
        match c {
            '%' => out.push_str("%25"),
            '/' => out.push_str("%2F"),
            '#' => out.push_str("%23"),
            ':' => out.push_str("%3A"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(seg: &str) -> String {
    seg.replace("%2F", "/")
        .replace("%23", "#")
        .replace("%3A", ":")
        .replace("%25", "%")
}

impl NodeId {
    /// The id of an L1 system.
    pub fn system(system: SystemKey) -> NodeId {
        NodeId::within(Level::System, system, Vec::new())
    }

    /// The id of a node below L1, at `path` inside `system`. Each element of
    /// `path` is one segment and may contain any character.
    pub fn within(level: Level, system: SystemKey, path: Vec<String>) -> NodeId {
        NodeId(std::sync::Arc::new(IdParts {
            level,
            system,
            path,
        }))
    }

    /// The node's level, as encoded in its id.
    pub fn level(&self) -> Level {
        self.0.level
    }

    /// The system the node belongs to.
    pub fn system_key(&self) -> &SystemKey {
        &self.0.system
    }

    /// The node's path inside its system.
    pub fn path(&self) -> &[String] {
        &self.0.path
    }
}

impl From<NodeId> for String {
    fn from(id: NodeId) -> String {
        id.to_string()
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dir: Vec<String> = self
            .0
            .system
            .dir
            .segments()
            .iter()
            .map(|s| escape(s))
            .collect();
        let path: Vec<String> = self.0.path.iter().map(|s| escape(s)).collect();
        write!(
            f,
            "L{}:{}:{}#{}",
            self.0.level.number(),
            self.0.system.ecosystem.tag(),
            dir.join("/"),
            path.join("/")
        )
    }
}

impl TryFrom<String> for NodeId {
    type Error = InvalidPath;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        let bad = || InvalidPath(s.clone());
        let (head, path) = s.split_once('#').ok_or_else(bad)?;
        let mut parts = head.splitn(3, ':');
        let level = match parts.next() {
            Some("L1") => Level::System,
            Some("L2") => Level::Module,
            Some("L3") => Level::Component,
            Some("L4") => Level::Code,
            _ => return Err(bad()),
        };
        let eco = parts.next().ok_or_else(bad)?;
        let ecosystem = Ecosystem::from_tag(eco).ok_or_else(bad)?;
        let dir_raw = parts.next().ok_or_else(bad)?;
        let split = |raw: &str| -> Vec<String> {
            if raw.is_empty() {
                Vec::new()
            } else {
                raw.split('/').map(unescape).collect()
            }
        };
        let dir = RepoPath::parse(&split(dir_raw).join("/")).map_err(|_| bad())?;
        let path = split(path);
        // L1 has no path; L3 and L4 always have one; an empty L2 path is a
        // crate's root module (DR 0008).
        let path_ok = match level {
            Level::System => path.is_empty(),
            Level::Module => true,
            Level::Component | Level::Code => !path.is_empty(),
        };
        if path.iter().any(String::is_empty) || !path_ok {
            return Err(bad());
        }
        let id = NodeId::within(level, SystemKey { ecosystem, dir }, path);
        if id.to_string() == s {
            Ok(id)
        } else {
            Err(bad())
        }
    }
}

/// What a node is, beyond its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    /// L1 system (its ecosystem is in the id).
    System,
    /// L1 holding files that belong to no manifest, so nothing disappears.
    Unowned,
    /// L2 module resolved by a language analyser.
    Module,
    /// L2 directory, used where no analyser exists (`unknown` confidence).
    Directory,
    /// L3 type or file-level group of functions.
    Component,
    /// L4 function or method.
    Function,
}

impl NodeKind {
    /// The only level this kind may appear at.
    pub fn level(self) -> Level {
        match self {
            NodeKind::System | NodeKind::Unowned => Level::System,
            NodeKind::Module | NodeKind::Directory => Level::Module,
            NodeKind::Component => Level::Component,
            NodeKind::Function => Level::Code,
        }
    }
}

/// One node of the model. L2 is flat within its system: a nested module
/// (`git/tree`) is its own L2 node whose path shows the nesting, because
/// drilling always moves exactly one of the four fixed levels (§3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identity.
    pub id: NodeId,
    /// What it is; must match the level in `id`.
    pub kind: NodeKind,
    /// Display name, as the code or manifest names it.
    pub name: String,
    /// The file or directory that defines it.
    pub source: RepoPath,
    /// The node one level up, or `None` for an L1 system.
    pub parent: Option<NodeId>,
    /// How sure the analyser is that this node exists as shown.
    pub confidence: Confidence,
    /// Who can use it from outside, where the language says so (a Rust
    /// module's `pub`). `None` when the analyser does not know or the idea
    /// does not apply, as for systems and directories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// Where an L3 or L4 node's code is, and fingerprints of it, for the
    /// diff. `None` above L3 and for files no analyser reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Code>,
}

/// The code of an L3 or L4 node: its lines in [`Node::source`], and hashes
/// of its tokens that ignore whitespace, comments and the node's own name
/// (DR 0008). Equal body hashes with different ids mean a move or rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Code {
    /// First and last line, 1-based and inclusive.
    pub lines: (u32, u32),
    /// The line the body starts on; lines before it are the signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_line: Option<u32>,
    /// Hash of everything outside the body: the public surface.
    pub signature_hash: u64,
    /// Hash of the body; `None` for an item without one, which is never
    /// paired as a move or rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<u64>,
}

/// How visible a node is outside its parent (§3.3: public surface changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Visible only where it is defined (Rust: no `pub`).
    Private,
    /// Visible to a limited scope (Rust: `pub(crate)`, `pub(super)`, `pub(in ..)`).
    Restricted,
    /// Part of the public surface (Rust: `pub`).
    Public,
}

/// Why a `depends-on` edge exists, where the manifest says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepKind {
    /// Needed by the built code (Cargo: `[dependencies]`).
    Normal,
    /// Needed only to build it (Cargo: `[build-dependencies]`).
    Build,
}

impl Node {
    /// The node's level (from its id).
    pub fn level(&self) -> Level {
        self.id.level()
    }
}

/// The kind of a directed edge (§3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeKind {
    /// An import or declared dependency.
    DependsOn,
    /// A call.
    Calls,
    /// Implements an interface or trait.
    Implements,
    /// Extends a type.
    Extends,
    /// Reads a data store or shared state.
    Reads,
    /// Writes a data store or shared state.
    Writes,
    /// Exposes public API.
    Exposes,
}

/// Identity of an edge: there is at most one edge per `(from, to, kind)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EdgeKey {
    /// Source node.
    pub from: NodeId,
    /// Target node.
    pub to: NodeId,
    /// What the edge means.
    pub kind: EdgeKind,
}

/// One directed edge between two nodes at the same level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// Its identity.
    #[serde(flatten)]
    pub key: EdgeKey,
    /// How sure the analyser is. When the same edge is added twice, the most
    /// certain observation wins; roll-up computes its own confidence.
    pub confidence: Confidence,
    /// For a rolled-up edge, the edges exactly one level down that it stands
    /// for. Empty when the edge was found directly at this level.
    pub evidence: BTreeSet<EdgeKey>,
    /// For a `depends-on` edge, why it exists. Empty when the analyser does
    /// not distinguish. Merging two observations of an edge unites these.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub dependency: BTreeSet<DepKind>,
}

/// Why a node or edge was rejected. The model is unchanged when this is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// A node with this id already exists.
    DuplicateNode(NodeId),
    /// The node's parent is not in the model.
    MissingParent(NodeId),
    /// The node's kind, parent level or parent system does not fit its id.
    Inconsistent(NodeId),
    /// An edge or evidence entry refers to a node that is not in the model.
    MissingNode(NodeId),
    /// An edge joins a node to itself, joins different levels, or cites
    /// evidence that is missing or not one level below its ends.
    InvalidEdge(Box<EdgeKey>),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::DuplicateNode(id) => write!(f, "duplicate node {id}"),
            ModelError::MissingParent(id) => write!(f, "parent of {id} is not in the model"),
            ModelError::Inconsistent(id) => {
                write!(f, "{id}: kind, level or parent does not fit the id")
            }
            ModelError::MissingNode(id) => write!(f, "refers to missing node {id}"),
            ModelError::InvalidEdge(k) => {
                write!(f, "invalid edge {} -> {} ({:?})", k.from, k.to, k.kind)
            }
        }
    }
}

impl std::error::Error for ModelError {}

/// The architecture model of one commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawModel", into = "RawModel")]
pub struct Model {
    nodes: BTreeMap<NodeId, Node>,
    children: BTreeMap<NodeId, BTreeSet<NodeId>>,
    edges: BTreeMap<EdgeKey, Edge>,
}

/// The serialised form: plain lists, re-validated on load.
#[derive(Serialize, Deserialize)]
struct RawModel {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl TryFrom<RawModel> for Model {
    type Error = ModelError;
    fn try_from(mut raw: RawModel) -> Result<Self, ModelError> {
        let mut m = Model::default();
        raw.nodes.sort_by_key(Node::level);
        for n in raw.nodes {
            m.add_node(n)?;
        }
        // Lower levels first, so evidence edges exist before the edges citing them.
        raw.edges
            .sort_by_key(|e| std::cmp::Reverse(e.key.from.level()));
        for e in raw.edges {
            m.add_edge(e)?;
        }
        Ok(m)
    }
}

impl From<Model> for RawModel {
    fn from(m: Model) -> RawModel {
        RawModel {
            nodes: m.nodes.into_values().collect(),
            edges: m.edges.into_values().collect(),
        }
    }
}

impl Model {
    /// Adds a node. Its kind must fit the level in its id. An L1 node has no
    /// parent; any other node's parent must already be present, exactly one
    /// level up, in the same system, with a path that is a prefix of its own.
    pub fn add_node(&mut self, node: Node) -> Result<(), ModelError> {
        let id = node.id.clone();
        if self.nodes.contains_key(&id) {
            return Err(ModelError::DuplicateNode(id));
        }
        let fits_kind = node.kind.level() == id.level()
            && (node.kind == NodeKind::Unowned)
                == (id.system_key().ecosystem == Ecosystem::Unowned && id.level() == Level::System);
        if !fits_kind {
            return Err(ModelError::Inconsistent(id));
        }
        match (&node.parent, id.level().up()) {
            (None, None) => {}
            (Some(parent), Some(expected)) => {
                let p = self
                    .nodes
                    .get(parent)
                    .ok_or_else(|| ModelError::MissingParent(id.clone()))?;
                let same_system = p.id.system_key() == id.system_key();
                if p.level() != expected || !same_system || !id.path().starts_with(p.id.path()) {
                    return Err(ModelError::Inconsistent(id));
                }
            }
            _ => return Err(ModelError::Inconsistent(id)),
        }
        if let Some(parent) = &node.parent {
            self.children
                .entry(parent.clone())
                .or_default()
                .insert(id.clone());
        }
        self.nodes.insert(id, node);
        Ok(())
    }

    /// Adds an edge between two different nodes at the same level. Evidence
    /// must name edges already in the model, exactly one level down, running
    /// from inside `from` to inside `to`. Adding an edge that exists merges
    /// the two: evidence is united and the more certain confidence kept.
    pub fn add_edge(&mut self, edge: Edge) -> Result<(), ModelError> {
        let k = &edge.key;
        for end in [&k.from, &k.to] {
            if !self.nodes.contains_key(end) {
                return Err(ModelError::MissingNode(end.clone()));
            }
        }
        if k.from == k.to || k.from.level() != k.to.level() {
            return Err(ModelError::InvalidEdge(Box::new(k.clone())));
        }
        for ev in &edge.evidence {
            let below = |outer: &NodeId, inner: &NodeId| {
                self.ancestor_at(inner, outer.level()) == Some(outer)
            };
            let ok = self.edges.contains_key(ev)
                && k.from.level().down() == Some(ev.from.level())
                && below(&k.from, &ev.from)
                && below(&k.to, &ev.to);
            if !ok {
                return Err(ModelError::InvalidEdge(Box::new(k.clone())));
            }
        }
        match self.edges.get_mut(k) {
            Some(existing) => {
                existing.confidence = existing.confidence.max(edge.confidence);
                existing.evidence.extend(edge.evidence);
                existing.dependency.extend(edge.dependency);
            }
            None => {
                self.edges.insert(k.clone(), edge);
            }
        }
        Ok(())
    }

    /// Removes a node, its descendants and every edge touching any of them.
    /// Returns false if the node was not present.
    pub fn remove_node(&mut self, id: &NodeId) -> bool {
        let Some(node) = self.nodes.remove(id) else {
            return false;
        };
        if let Some(parent) = &node.parent
            && let Some(siblings) = self.children.get_mut(parent)
        {
            siblings.remove(id);
        }
        for child in self.children.remove(id).unwrap_or_default() {
            self.remove_node(&child);
        }
        self.edges.retain(|k, _| &k.from != id && &k.to != id);
        for e in self.edges.values_mut() {
            e.evidence.retain(|ev| &ev.from != id && &ev.to != id);
        }
        true
    }

    /// Builds a model from nodes and edges that are already known to form a
    /// valid model, such as one this crate wrote and a checksum protects,
    /// without checking them one by one. The maps are built in bulk.
    #[must_use]
    pub fn from_trusted_parts(nodes: Vec<Node>, edges: Vec<Edge>) -> Model {
        let mut children: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
        for n in &nodes {
            if let Some(p) = &n.parent {
                children.entry(p.clone()).or_default().insert(n.id.clone());
            }
        }
        Model {
            nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
            children,
            edges: edges.into_iter().map(|e| (e.key.clone(), e)).collect(),
        }
    }

    /// Replaces a node's attributes: its name, source, confidence, visibility
    /// and code. Its id, kind and parent must stay the same, so nothing that
    /// refers to it changes.
    ///
    /// # Errors
    /// [`ModelError::Inconsistent`] if there is no such node, or its kind or
    /// parent would change. The model is then unchanged.
    pub fn replace_node(&mut self, node: Node) -> Result<(), ModelError> {
        match self.nodes.get_mut(&node.id) {
            Some(old) if old.kind == node.kind && old.parent == node.parent => {
                *old = node;
                Ok(())
            }
            _ => Err(ModelError::Inconsistent(node.id)),
        }
    }

    /// Removes an edge. Returns false if it was not present.
    pub fn remove_edge(&mut self, key: &EdgeKey) -> bool {
        self.edges.remove(key).is_some()
    }

    fn ancestor_at<'a>(&'a self, id: &'a NodeId, level: Level) -> Option<&'a NodeId> {
        let mut cur = id;
        while cur.level() > level {
            cur = self.nodes.get(cur)?.parent.as_ref()?;
        }
        (cur.level() == level).then_some(cur)
    }

    /// Looks up a node.
    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Looks up an edge.
    pub fn edge(&self, key: &EdgeKey) -> Option<&Edge> {
        self.edges.get(key)
    }

    /// All nodes, ordered by id.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    /// All nodes at one level, ordered by id.
    pub fn level(&self, level: Level) -> impl Iterator<Item = &Node> {
        self.nodes.values().filter(move |n| n.level() == level)
    }

    /// The direct children of a node, ordered by id, from an index.
    pub fn children<'a>(&'a self, id: &NodeId) -> impl Iterator<Item = &'a Node> + 'a {
        let ids = self.children.get(id).into_iter().flatten();
        ids.filter_map(|c| self.nodes.get(c))
    }

    /// All edges, ordered by key.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.edges.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(dir: &str) -> SystemKey {
        SystemKey {
            ecosystem: Ecosystem::Cargo,
            dir: RepoPath::parse(dir).unwrap(),
        }
    }

    fn system(dir: &str) -> Node {
        Node {
            id: NodeId::system(key(dir)),
            kind: NodeKind::System,
            name: dir.rsplit('/').next().unwrap_or(dir).into(),
            source: RepoPath::parse(&format!("{dir}/Cargo.toml")).unwrap(),
            parent: None,
            confidence: Confidence::Exact,
            visibility: None,
            code: None,
        }
    }

    fn module(dir: &str, path: &[&str]) -> Node {
        Node {
            id: NodeId::within(
                Level::Module,
                key(dir),
                path.iter().map(|s| s.to_string()).collect(),
            ),
            kind: NodeKind::Module,
            name: path.last().copied().unwrap_or("").into(),
            source: RepoPath::parse(dir).unwrap(),
            parent: Some(NodeId::system(key(dir))),
            confidence: Confidence::Exact,
            visibility: Some(Visibility::Public),
            code: None,
        }
    }

    #[test]
    fn replace_node_keeps_identity_and_place() {
        let mut m = Model::default();
        m.add_node(system("a")).unwrap();
        m.add_node(module("a", &["x"])).unwrap();
        let mut changed = module("a", &["x"]);
        changed.name = "renamed".into();
        changed.visibility = Some(Visibility::Private);
        assert_eq!(m.replace_node(changed.clone()), Ok(()));
        assert_eq!(m.node(&changed.id), Some(&changed));
        assert_eq!(m.children(&NodeId::system(key("a"))).count(), 1);

        let mut moved = changed.clone();
        moved.parent = None;
        assert!(m.replace_node(moved).is_err(), "parent must stay");
        let mut kind = changed.clone();
        kind.kind = NodeKind::Directory;
        assert!(m.replace_node(kind).is_err(), "kind must stay");
        assert!(m.replace_node(module("a", &["y"])).is_err(), "must exist");
        assert_eq!(m.node(&changed.id), Some(&changed), "unchanged on error");
    }

    fn edge(from: &Node, to: &Node, evidence: &[EdgeKey]) -> Edge {
        Edge {
            key: EdgeKey {
                from: from.id.clone(),
                to: to.id.clone(),
                kind: EdgeKind::DependsOn,
            },
            confidence: Confidence::Exact,
            evidence: evidence.iter().cloned().collect(),
            dependency: BTreeSet::from([DepKind::Normal]),
        }
    }

    #[test]
    fn merging_an_edge_unites_its_dependency_kinds() {
        let mut m = Model::default();
        let (a, b) = (system("a"), system("b"));
        m.add_node(a.clone()).unwrap();
        m.add_node(b.clone()).unwrap();
        m.add_edge(edge(&a, &b, &[])).unwrap();
        let mut build = edge(&a, &b, &[]);
        build.dependency = BTreeSet::from([DepKind::Build]);
        m.add_edge(build).unwrap();
        let e = m.edges().next().unwrap();
        assert_eq!(
            e.dependency,
            BTreeSet::from([DepKind::Normal, DepKind::Build])
        );
    }

    #[test]
    fn visibility_and_dependency_are_optional_in_json() {
        let mut m = Model::default();
        let (a, b) = (system("a"), system("b"));
        m.add_node(a.clone()).unwrap();
        m.add_node(b.clone()).unwrap();
        m.add_node(module("a", &["x"])).unwrap();
        m.add_edge(edge(&a, &b, &[])).unwrap();
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains(r#""visibility":"public""#), "{json}");
        assert_eq!(serde_json::from_str::<Model>(&json).unwrap(), m);
        // Documents written before these fields existed still load.
        let old = json
            .replace(r#","visibility":"public""#, "")
            .replace(r#","dependency":["normal"]"#, "");
        let loaded = serde_json::from_str::<Model>(&old).unwrap();
        assert!(loaded.edges().all(|e| e.dependency.is_empty()));
        assert!(loaded.nodes().all(|n| n.visibility.is_none()));
    }

    #[test]
    fn ids_never_collide_across_separators_ecosystems_or_directories() {
        let a = NodeId::within(
            Level::Module,
            key("example.com/a"),
            vec!["b".into(), "c".into()],
        );
        let b = NodeId::within(Level::Module, key("example.com/a/b"), vec!["c".into()]);
        let c = NodeId::within(Level::Module, key("x"), vec!["b/c".into()]);
        let d = NodeId::within(Level::Module, key("x"), vec!["b".into(), "c".into()]);
        assert_ne!(a.to_string(), b.to_string());
        assert_ne!(c.to_string(), d.to_string());
        let npm = NodeId::system(SystemKey {
            ecosystem: Ecosystem::Npm,
            dir: RepoPath::parse("core").unwrap(),
        });
        assert_ne!(npm, NodeId::system(key("core")));
        assert_ne!(NodeId::system(key("a/core")), NodeId::system(key("b/core")));
    }

    #[test]
    fn id_text_round_trips_and_rejects_junk() {
        for id in [
            NodeId::system(key("")),
            // A crate's root module.
            NodeId::within(Level::Module, key("crates/x"), Vec::new()),
            NodeId::within(
                Level::Code,
                key("crates/x"),
                vec!["a:b#c%d/e".into(), "f".into()],
            ),
        ] {
            assert_eq!(NodeId::try_from(id.to_string()).unwrap(), id);
        }
        for bad in [
            "garbage",
            "L9:cargo:#",
            "L1:cargo:x#y",
            "L3:cargo:x#",
            "L4:cargo:x#",
            "L2:cargo:x#a//b",
            "L1:cargo:a/../b#",
            "L1:cargo:a%2fb#",
        ] {
            assert!(NodeId::try_from(bad.to_string()).is_err(), "{bad}");
        }
        assert!(RepoPath::parse("a\\b").is_err() && RepoPath::parse("a//b").is_err());
    }

    #[test]
    fn nodes_must_fit_their_id_kind_parent_and_system() {
        let mut m = Model::default();
        let git = module("crates/engine", &["git"]);
        assert_eq!(
            m.add_node(git.clone()),
            Err(ModelError::MissingParent(git.id.clone()))
        );
        m.add_node(system("crates/engine")).unwrap();
        m.add_node(system("crates/cli")).unwrap();
        m.add_node(git).unwrap();
        assert!(matches!(
            m.add_node(system("crates/engine")),
            Err(ModelError::DuplicateNode(_))
        ));

        let mut wrong_kind = module("crates/engine", &["a"]);
        wrong_kind.kind = NodeKind::Function;
        let mut wrong_system = module("crates/engine", &["b"]);
        wrong_system.parent = Some(NodeId::system(key("crates/cli")));
        let mut l1_with_parent = system("crates/x");
        l1_with_parent.parent = Some(NodeId::system(key("crates/cli")));
        let mut unowned_cargo = system("crates/y");
        unowned_cargo.kind = NodeKind::Unowned;
        for bad in [wrong_kind, wrong_system, l1_with_parent, unowned_cargo] {
            assert!(matches!(m.add_node(bad), Err(ModelError::Inconsistent(_))));
        }
        assert_eq!(
            m.nodes().count(),
            3,
            "rejected nodes leave the model unchanged"
        );
    }

    #[test]
    fn edges_are_keyed_merged_and_checked() {
        let mut m = Model::default();
        let (cli, eng) = (system("crates/cli"), system("crates/engine"));
        let (main, git) = (
            module("crates/cli", &["main"]),
            module("crates/engine", &["git"]),
        );
        for n in [cli.clone(), eng.clone(), main.clone(), git.clone()] {
            m.add_node(n).unwrap();
        }
        assert!(matches!(
            m.add_edge(edge(&cli, &cli, &[])),
            Err(ModelError::InvalidEdge(_))
        ));
        assert!(matches!(
            m.add_edge(edge(&cli, &git, &[])),
            Err(ModelError::InvalidEdge(_))
        ));
        let low = edge(&main, &git, &[]);
        let ghost = EdgeKey {
            kind: EdgeKind::Calls,
            ..low.key.clone()
        };
        assert!(matches!(
            m.add_edge(edge(&cli, &eng, &[ghost])),
            Err(ModelError::InvalidEdge(_))
        ));
        m.add_edge(low.clone()).unwrap();
        let mut inferred = edge(&cli, &eng, &[]);
        inferred.confidence = Confidence::Inferred;
        m.add_edge(inferred).unwrap();
        m.add_edge(edge(&cli, &eng, std::slice::from_ref(&low.key)))
            .unwrap();
        let up = m
            .edge(&EdgeKey {
                from: cli.id.clone(),
                to: eng.id.clone(),
                kind: EdgeKind::DependsOn,
            })
            .unwrap();
        assert_eq!((up.confidence, up.evidence.len()), (Confidence::Exact, 1));
        assert_eq!(m.edges().count(), 2, "one edge per (from, to, kind)");
        let backwards = edge(&eng, &cli, std::slice::from_ref(&low.key));
        assert!(
            matches!(m.add_edge(backwards), Err(ModelError::InvalidEdge(_))),
            "evidence must run the same way"
        );
    }

    #[test]
    fn removal_cascades_and_children_are_indexed() {
        let mut m = Model::default();
        let (cli, eng) = (system("crates/cli"), system("crates/engine"));
        let (main, git) = (
            module("crates/cli", &["main"]),
            module("crates/engine", &["git"]),
        );
        for n in [
            cli.clone(),
            eng.clone(),
            main.clone(),
            git.clone(),
            module("crates/engine", &["a"]),
        ] {
            m.add_node(n).unwrap();
        }
        let names: Vec<_> = m.children(&eng.id).map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["a", "git"]);
        m.add_edge(edge(&main, &git, &[])).unwrap();
        m.add_edge(edge(&cli, &eng, &[edge(&main, &git, &[]).key]))
            .unwrap();
        assert!(m.remove_node(&eng.id));
        assert_eq!(m.nodes().count(), 2);
        assert_eq!(m.edges().count(), 0);
        assert!(!m.remove_node(&eng.id));
    }

    #[test]
    fn json_is_deterministic_documented_and_validated_on_load() {
        let build = |rev: bool| {
            let mut m = Model::default();
            let mut ns = vec![system("crates/cli"), system("crates/engine")];
            if rev {
                ns.reverse();
            }
            for n in ns {
                m.add_node(n).unwrap();
            }
            m.add_edge(edge(&system("crates/cli"), &system("crates/engine"), &[]))
                .unwrap();
            m
        };
        let json = serde_json::to_string_pretty(&build(false)).unwrap();
        assert_eq!(json, serde_json::to_string_pretty(&build(true)).unwrap());
        let doc = include_str!("../../../docs/model-json.md");
        let example = doc
            .split("```json\n")
            .nth(1)
            .and_then(|s| s.split("\n```").next())
            .unwrap();
        assert_eq!(
            json, example,
            "docs/model-json.md must show the real serialised form"
        );
        assert_eq!(serde_json::from_str::<Model>(&json).unwrap(), build(false));

        let dangling = json.replace(
            r#""to": "L1:cargo:crates/engine#""#,
            r#""to": "L1:cargo:nowhere#""#,
        );
        assert!(serde_json::from_str::<Model>(&dangling).is_err());
        let duplicate = json.replacen("crates/engine#", "crates/cli#", 1);
        assert!(serde_json::from_str::<Model>(&duplicate).is_err());
    }
}
