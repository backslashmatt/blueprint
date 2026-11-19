//! The architectural diff of two models (REQUIREMENTS §3.3): the types.
//! The engine computes them (`blueprint_engine::diff`); front ends render
//! them through `protocol`.

use serde::Serialize;

use crate::{EdgeKey, NodeId};

/// How one node changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "change", rename_all = "kebab-case")]
pub enum NodeChange {
    /// Only in head.
    Added {
        /// The node.
        id: NodeId,
    },
    /// Only in base.
    Removed {
        /// The node.
        id: NodeId,
    },
    /// The same body under a different parent.
    Moved {
        /// The node's id in base.
        from: NodeId,
        /// Its id in head.
        to: NodeId,
    },
    /// The same body and parent under a different name.
    Renamed {
        /// The node's id in base.
        from: NodeId,
        /// Its id in head.
        to: NodeId,
    },
    /// A visible node whose signature or visibility changed.
    Surface {
        /// The node.
        id: NodeId,
    },
    /// Only the body changed, and no edge from or to the node did.
    Contained {
        /// The node.
        id: NodeId,
    },
    /// Anything else: a private signature, a body change that adds or
    /// removes edges, or a change of confidence or source.
    Changed {
        /// The node.
        id: NodeId,
    },
}

/// Something a reviewer should see before the detail.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "finding", rename_all = "kebab-case")]
pub enum Finding {
    /// A new edge between two systems (L1) or two modules (L2).
    NewDependency {
        /// The edge.
        edge: EdgeKey,
    },
    /// A `depends-on` cycle at one level, closed by edges new in head.
    NewCycle {
        /// The level: 1 or 2.
        level: u8,
        /// The nodes on the cycle, sorted.
        nodes: Vec<NodeId>,
        /// The cycle's edges that are new in head, sorted.
        new_edges: Vec<EdgeKey>,
    },
}

/// The architectural diff of two models.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Diff {
    /// The findings, sorted.
    pub findings: Vec<Finding>,
    /// Every node that changed, sorted.
    pub nodes: Vec<NodeChange>,
    /// Edges only in head, sorted.
    pub added_edges: Vec<EdgeKey>,
    /// Edges only in base, sorted.
    pub removed_edges: Vec<EdgeKey>,
    /// Each node's change weight, sorted by id; empty until the engine weighs the diff.
    pub weights: Vec<Weight>,
}

impl Diff {
    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.added_edges.is_empty() && self.removed_edges.is_empty()
    }
}

/// Lines changed in one node and everything under it (DR 0008 decision 6:
/// change weight): lines removed from base plus lines added in head. A line
/// before the node's body, or the line the node starts on, is a signature
/// line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Lines {
    /// Changed lines in signatures.
    pub signature: u32,
    /// Changed lines in bodies, and anything else.
    pub body: u32,
}

impl Lines {
    /// Adds `other`'s lines to these.
    pub fn add(&mut self, other: Lines) {
        self.signature += other.signature;
        self.body += other.body;
    }

    /// Whether no line changed.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.signature == 0 && self.body == 0
    }
}

/// One node's change weight.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Weight {
    /// The node.
    pub id: NodeId,
    /// Its changed lines, its descendants' included.
    #[serde(flatten)]
    pub lines: Lines,
    /// A hash of the text of those lines, on each side, without their line
    /// numbers: what changed, so a review mark applies to this change and
    /// no other (DR 0011). Zero when unknown. Not serialised.
    #[serde(skip)]
    pub digest: u64,
}

/// One side of a [`FileDiff`]: the file's lines and their coloured tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileText {
    /// The lines, without line endings; invalid UTF-8 replaced.
    pub lines: Vec<String>,
    /// For each line, its coloured tokens as byte ranges within the line,
    /// sorted and not overlapping.
    pub highlights: Vec<Vec<LineSpan>>,
}

/// A coloured token within one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LineSpan {
    /// First byte in the line.
    pub start: u32,
    /// One past the last byte in the line.
    pub end: u32,
    /// What it is.
    pub class: crate::lang::TokenClass,
}

/// How one row of a side-by-side diff changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RowKind {
    /// The same line on both sides.
    Same,
    /// A line replaced by another.
    Changed,
    /// Only in base.
    Removed,
    /// Only in head.
    Added,
}

/// One row of a side-by-side diff: a line on either side, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiffRow {
    /// The base line's index (from 0), if this row has one.
    pub base: Option<u32>,
    /// The head line's index (from 0), if this row has one.
    pub head: Option<u32>,
    /// How it changed.
    pub kind: RowKind,
}

/// A file's side-by-side diff for the code view at L4 (§5.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FileDiff {
    /// The file in base; empty where it does not exist.
    pub base: FileText,
    /// The file in head; empty where it does not exist.
    pub head: FileText,
    /// Every row, top to bottom.
    pub rows: Vec<DiffRow>,
}
