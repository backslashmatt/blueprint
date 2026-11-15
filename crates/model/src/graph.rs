//! Nodes, edges and the model that holds them (REQUIREMENTS §3.1–3.2).
//!
//! Identity is derived from *where* a node is (its system and path), never
//! from its content, so the same module has the same [`NodeId`] at two
//! commits and the diff (M2) can tell "changed" from "added and removed".
//! Every collection is ordered, so the JSON form is deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Confidence, Level};

/// Stable identity of a node: `L<level>:<system>[/<path within the system>]`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(String);

impl NodeId {
    /// The id of an L1 system, named as its manifest names it.
    pub fn system(name: &str) -> NodeId {
        NodeId(format!("L1:{name}"))
    }

    /// The id of a node below L1: `path` is its location inside `system`,
    /// segments joined by `/` (for example `parser/lexer` for a Rust module).
    pub fn within(level: Level, system: &str, path: &str) -> NodeId {
        NodeId(format!("L{}:{system}/{path}", level.number()))
    }

    /// The id as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a node is, beyond its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    /// L1 from a `Cargo.toml` package.
    CargoPackage,
    /// L1 from a `package.json` package.
    NpmPackage,
    /// L1 from a `go.mod` module.
    GoModule,
    /// L1 from a `*.csproj` project.
    DotnetProject,
    /// L1 from an Nx `project.json`.
    NxProject,
    /// L1 from a Bazel `BUILD` package.
    BazelPackage,
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

/// One node of the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identity.
    pub id: NodeId,
    /// Its level in the hierarchy.
    pub level: Level,
    /// What it is.
    pub kind: NodeKind,
    /// Display name.
    pub name: String,
    /// Repository-relative path of the file or directory that defines it.
    pub path: String,
    /// The node one level up, or `None` for an L1 system.
    pub parent: Option<NodeId>,
    /// How sure the analyser is that this node exists as shown.
    pub confidence: Confidence,
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

/// One directed edge. Rolled-up edges list the lower-level edges they stand for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Edge {
    /// Source node.
    pub from: NodeId,
    /// Target node.
    pub to: NodeId,
    /// What the edge means.
    pub kind: EdgeKind,
    /// How sure the analyser is.
    pub confidence: Confidence,
    /// The lower-level edges this one aggregates, as `(from, to)` pairs; empty
    /// when the edge was found directly at this level.
    pub evidence: BTreeSet<(NodeId, NodeId)>,
}

/// Why a node or edge could not be added. The model is unchanged when this is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// A node with this id already exists.
    DuplicateNode(NodeId),
    /// The node's parent is not in the model.
    MissingParent(NodeId),
    /// An L1 node has a parent, a lower node has none, or the parent is not exactly one level up.
    LevelMismatch(NodeId),
    /// An edge refers to a node that is not in the model.
    DanglingEdge(NodeId),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::DuplicateNode(id) => write!(f, "duplicate node {id}"),
            ModelError::MissingParent(id) => write!(f, "parent of {id} is not in the model"),
            ModelError::LevelMismatch(id) => {
                write!(f, "{id} is not exactly one level below its parent")
            }
            ModelError::DanglingEdge(id) => write!(f, "edge refers to missing node {id}"),
        }
    }
}

impl std::error::Error for ModelError {}

/// The architecture model of one commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    nodes: BTreeMap<NodeId, Node>,
    edges: BTreeSet<Edge>,
}

impl Model {
    /// Adds a node whose parent, if any, is already present one level up.
    pub fn add_node(&mut self, node: Node) -> Result<(), ModelError> {
        if self.nodes.contains_key(&node.id) {
            return Err(ModelError::DuplicateNode(node.id));
        }
        match (&node.parent, node.level.up()) {
            (None, None) => {}
            (Some(parent), Some(expected)) => {
                let p = self
                    .nodes
                    .get(parent)
                    .ok_or_else(|| ModelError::MissingParent(node.id.clone()))?;
                if p.level != expected {
                    return Err(ModelError::LevelMismatch(node.id));
                }
            }
            _ => return Err(ModelError::LevelMismatch(node.id)),
        }
        self.nodes.insert(node.id.clone(), node);
        Ok(())
    }

    /// Adds an edge between two nodes already in the model. Adding the same
    /// edge twice keeps one copy.
    pub fn add_edge(&mut self, edge: Edge) -> Result<(), ModelError> {
        for end in [&edge.from, &edge.to] {
            if !self.nodes.contains_key(end) {
                return Err(ModelError::DanglingEdge(end.clone()));
            }
        }
        self.edges.insert(edge);
        Ok(())
    }

    /// Looks up a node.
    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// All nodes, ordered by id.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    /// All nodes at one level, ordered by id.
    pub fn level(&self, level: Level) -> impl Iterator<Item = &Node> {
        self.nodes.values().filter(move |n| n.level == level)
    }

    /// The direct children of a node, ordered by id.
    pub fn children<'a>(&'a self, id: &'a NodeId) -> impl Iterator<Item = &'a Node> + 'a {
        self.nodes
            .values()
            .filter(move |n| n.parent.as_ref() == Some(id))
    }

    /// All edges, in a deterministic order.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.edges.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(name: &str) -> Node {
        Node {
            id: NodeId::system(name),
            level: Level::System,
            kind: NodeKind::CargoPackage,
            name: name.into(),
            path: format!("crates/{name}/Cargo.toml"),
            parent: None,
            confidence: Confidence::Exact,
        }
    }

    fn module(system: &str, path: &str) -> Node {
        Node {
            id: NodeId::within(Level::Module, system, path),
            level: Level::Module,
            kind: NodeKind::Module,
            name: path.rsplit('/').next().unwrap_or(path).into(),
            path: format!("crates/{system}/src/{path}.rs"),
            parent: Some(NodeId::system(system)),
            confidence: Confidence::Exact,
        }
    }

    fn depends(from: &str, to: &str) -> Edge {
        Edge {
            from: NodeId::system(from),
            to: NodeId::system(to),
            kind: EdgeKind::DependsOn,
            confidence: Confidence::Exact,
            evidence: BTreeSet::new(),
        }
    }

    #[test]
    fn ids_depend_on_location_not_content() {
        assert_eq!(NodeId::system("engine").as_str(), "L1:engine");
        assert_eq!(
            NodeId::within(Level::Module, "engine", "git/tree").as_str(),
            "L2:engine/git/tree"
        );
        let mut changed = module("engine", "git");
        changed.confidence = Confidence::Inferred;
        assert_eq!(changed.id, module("engine", "git").id);
    }

    #[test]
    fn hierarchy_is_enforced() {
        let mut m = Model::default();
        assert_eq!(
            m.add_node(module("engine", "git")),
            Err(ModelError::MissingParent(module("engine", "git").id))
        );
        m.add_node(system("engine")).unwrap();
        m.add_node(module("engine", "git")).unwrap();
        assert!(matches!(
            m.add_node(system("engine")),
            Err(ModelError::DuplicateNode(_))
        ));

        let mut orphan_l1 = system("cli");
        orphan_l1.parent = Some(NodeId::system("engine"));
        assert!(matches!(
            m.add_node(orphan_l1),
            Err(ModelError::LevelMismatch(_))
        ));

        let mut skips_a_level = module("engine", "deep");
        skips_a_level.level = Level::Component;
        assert!(matches!(
            m.add_node(skips_a_level),
            Err(ModelError::LevelMismatch(_))
        ));
        assert_eq!(
            m.nodes().count(),
            2,
            "failed adds leave the model unchanged"
        );
    }

    #[test]
    fn edges_need_both_ends_and_are_deduplicated() {
        let mut m = Model::default();
        m.add_node(system("cli")).unwrap();
        assert_eq!(
            m.add_edge(depends("cli", "engine")),
            Err(ModelError::DanglingEdge(NodeId::system("engine")))
        );
        m.add_node(system("engine")).unwrap();
        m.add_edge(depends("cli", "engine")).unwrap();
        m.add_edge(depends("cli", "engine")).unwrap();
        assert_eq!(m.edges().count(), 1);
    }

    #[test]
    fn queries_are_ordered_and_scoped() {
        let mut m = Model::default();
        for s in ["zeta", "alpha"] {
            m.add_node(system(s)).unwrap();
        }
        for p in ["b", "a"] {
            m.add_node(module("alpha", p)).unwrap();
        }
        let l1: Vec<_> = m.level(Level::System).map(|n| n.name.as_str()).collect();
        assert_eq!(l1, ["alpha", "zeta"]);
        let alpha = NodeId::system("alpha");
        let kids: Vec<_> = m.children(&alpha).map(|n| n.name.as_str()).collect();
        assert_eq!(kids, ["a", "b"]);
        assert!(m.node(&NodeId::system("zeta")).is_some());
    }

    #[test]
    fn json_is_deterministic_and_round_trips() {
        let build = |order: &[&str]| {
            let mut m = Model::default();
            for s in order {
                m.add_node(system(s)).unwrap();
            }
            m.add_edge(depends("cli", "engine")).unwrap();
            m
        };
        let a = serde_json::to_string(&build(&["engine", "cli"])).unwrap();
        let b = serde_json::to_string(&build(&["cli", "engine"])).unwrap();
        assert_eq!(a, b, "insertion order must not change the JSON");
        assert!(
            a.contains(r#""level":"L1""#)
                && a.contains(r#""kind":"depends-on""#)
                && a.contains(r#""confidence":"exact""#),
            "{a}"
        );
        let back: Model = serde_json::from_str(&a).unwrap();
        assert_eq!(back, build(&["cli", "engine"]));
    }
}
