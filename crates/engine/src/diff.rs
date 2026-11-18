//! The architectural diff of two models (REQUIREMENTS §3.3, DR 0008
//! decision 6): what changed in nodes and edges, and the findings a
//! reviewer should see first.
//!
//! - Nodes are compared by id. A removed and an added node left unmatched
//!   are paired when they have the same kind and the same body hash: a
//!   **move** if their parents differ, a **rename** if only the name does.
//! - A node in both models is **surface**-changed when it is visible outside
//!   its parent (`pub` or restricted) and its signature or visibility
//!   changed; **contained** when only its body changed and no edge from or
//!   to it was added or removed; otherwise **changed**.
//! - Edges are compared by `(from, to, kind)`.
//! - **Findings:** a new edge at L1 or L2 (a new dependency across a
//!   boundary), and a new `depends-on` cycle at L1 or L2 that a new edge
//!   closes.
//!
//! Everything is sorted, so the same two models always give the same diff.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{EdgeKey, EdgeKind, Level, Model, Node, NodeId, Visibility};
use serde::Serialize;

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
}

impl Diff {
    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.added_edges.is_empty() && self.removed_edges.is_empty()
    }
}

/// The diff from `base` to `head`.
#[must_use]
pub fn diff(base: &Model, head: &Model) -> Diff {
    let base_edges: BTreeSet<&EdgeKey> = base.edges().map(|e| &e.key).collect();
    let head_edges: BTreeSet<&EdgeKey> = head.edges().map(|e| &e.key).collect();
    let added_edges: Vec<EdgeKey> = head_edges
        .difference(&base_edges)
        .map(|k| (*k).clone())
        .collect();
    let removed_edges: Vec<EdgeKey> = base_edges
        .difference(&head_edges)
        .map(|k| (*k).clone())
        .collect();
    // Nodes an added or removed edge touches: their body change is not
    // contained.
    let mut touched: BTreeSet<&NodeId> = BTreeSet::new();
    for k in added_edges.iter().chain(&removed_edges) {
        touched.insert(&k.from);
        touched.insert(&k.to);
    }

    let mut nodes = Vec::new();
    let mut removed: Vec<&Node> = Vec::new();
    for b in base.nodes() {
        match head.node(&b.id) {
            None => removed.push(b),
            Some(h) => {
                if let Some(change) = changed(b, h, touched.contains(&b.id)) {
                    nodes.push(change);
                }
            }
        }
    }
    let added: Vec<&Node> = head
        .nodes()
        .filter(|h| base.node(&h.id).is_none())
        .collect();
    nodes.extend(pair(&removed, &added));

    let mut findings: Vec<Finding> = added_edges
        .iter()
        .filter(|k| matches!(k.from.level(), Level::System | Level::Module))
        .map(|k| Finding::NewDependency { edge: k.clone() })
        .collect();
    let new: BTreeSet<&EdgeKey> = added_edges.iter().collect();
    for level in [Level::System, Level::Module] {
        findings.extend(new_cycles(head, level, &new));
    }

    nodes.sort();
    findings.sort();
    Diff {
        findings,
        nodes,
        added_edges,
        removed_edges,
    }
}

/// How a node in both models changed, if it did.
fn changed(b: &Node, h: &Node, touched: bool) -> Option<NodeChange> {
    if b == h {
        return None;
    }
    let id = h.id.clone();
    let visible = |n: &Node| {
        matches!(
            n.visibility,
            Some(Visibility::Public | Visibility::Restricted)
        )
    };
    let (bc, hc) = (b.code.as_ref(), h.code.as_ref());
    let signature = bc.map(|c| c.signature_hash) != hc.map(|c| c.signature_hash);
    let surface = signature || b.visibility != h.visibility;
    if surface && (visible(b) || visible(h)) {
        return Some(NodeChange::Surface { id });
    }
    let body_only = !surface
        && b.name == h.name
        && b.kind == h.kind
        && b.source == h.source
        && b.confidence == h.confidence
        && bc.map(|c| c.body_hash) != hc.map(|c| c.body_hash);
    if body_only && !touched {
        return Some(NodeChange::Contained { id });
    }
    // Only lines moved: the same signature and body elsewhere in the file
    // is not a change a reviewer needs.
    let same_content = !surface
        && bc.map(|c| (c.body_hash, c.signature_hash))
            == hc.map(|c| (c.body_hash, c.signature_hash))
        && b.name == h.name
        && b.kind == h.kind
        && b.source == h.source
        && b.confidence == h.confidence
        && b.visibility == h.visibility;
    if same_content {
        return None;
    }
    Some(NodeChange::Changed { id })
}

/// Pairs removed and added nodes with the same kind and body hash as moves
/// and renames; the rest stay removed and added. Pairing is greedy in id
/// order, so it is deterministic.
fn pair(removed: &[&Node], added: &[&Node]) -> Vec<NodeChange> {
    type Key = (blueprint_model::NodeKind, u64);
    let body = |n: &Node| -> Option<Key> { Some((n.kind, n.code.as_ref()?.body_hash?)) };
    let mut by_body: BTreeMap<Key, Vec<&Node>> = BTreeMap::new();
    for a in added {
        if let Some(k) = body(a) {
            by_body.entry(k).or_default().push(a);
        }
    }
    let mut out = Vec::new();
    let mut paired: BTreeSet<&NodeId> = BTreeSet::new();
    for r in removed {
        let candidate = body(r)
            .and_then(|k| by_body.get(&k))
            .and_then(|list| list.iter().find(|a| !paired.contains(&a.id)));
        match candidate {
            Some(a) => {
                paired.insert(&a.id);
                let (from, to) = (r.id.clone(), a.id.clone());
                out.push(if r.parent == a.parent {
                    NodeChange::Renamed { from, to }
                } else {
                    NodeChange::Moved { from, to }
                });
            }
            None => out.push(NodeChange::Removed { id: r.id.clone() }),
        }
    }
    for a in added {
        if !paired.contains(&a.id) {
            out.push(NodeChange::Added { id: a.id.clone() });
        }
    }
    out
}

/// The `depends-on` cycles at `level` in `head` that contain an edge in
/// `new`: each strongly connected component of more than one node.
fn new_cycles(head: &Model, level: Level, new: &BTreeSet<&EdgeKey>) -> Vec<Finding> {
    let edges: Vec<&EdgeKey> = head
        .edges()
        .map(|e| &e.key)
        .filter(|k| k.kind == EdgeKind::DependsOn && k.from.level() == level)
        .collect();
    if !edges.iter().any(|k| new.contains(k)) {
        return Vec::new();
    }
    let ids: BTreeSet<&NodeId> = edges.iter().flat_map(|k| [&k.from, &k.to]).collect();
    let index: BTreeMap<&NodeId, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut adj = vec![Vec::new(); ids.len()];
    for k in &edges {
        adj[index[&k.from]].push(index[&k.to]);
    }
    let names: Vec<&NodeId> = ids.into_iter().collect();
    let mut out = Vec::new();
    for scc in tarjan(&adj).into_iter().filter(|c| c.len() > 1) {
        let members: BTreeSet<usize> = scc.iter().copied().collect();
        let mut new_edges: Vec<EdgeKey> = edges
            .iter()
            .filter(|k| new.contains(*k))
            .filter(|k| members.contains(&index[&k.from]) && members.contains(&index[&k.to]))
            .map(|k| (*k).clone())
            .collect();
        if new_edges.is_empty() {
            continue;
        }
        new_edges.sort();
        let mut nodes: Vec<NodeId> = scc.iter().map(|&i| names[i].clone()).collect();
        nodes.sort();
        out.push(Finding::NewCycle {
            level: if level == Level::System { 1 } else { 2 },
            nodes,
            new_edges,
        });
    }
    out
}

/// Tarjan's strongly connected components, iteratively, so a long chain of
/// modules cannot exhaust the stack.
fn tarjan(adj: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const NONE: usize = usize::MAX;
    let n = adj.len();
    let (mut index, mut low) = (vec![NONE; n], vec![0; n]);
    let mut on_stack = vec![false; n];
    let (mut stack, mut out, mut next) = (Vec::new(), Vec::new(), 0);
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        // (node, position in its adjacency list)
        let mut call = vec![(root, 0usize)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (v, ref mut i)) = call.last_mut() {
            if let Some(&w) = adj[v].get(*i) {
                *i += 1;
                if index[w] == NONE {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            call.pop();
            if let Some(&(parent, _)) = call.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == index[v] {
                let mut scc = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                out.push(scc);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::tarjan;

    #[test]
    fn tarjan_finds_cycles_and_singletons() {
        // 0 -> 1 -> 2 -> 0, 2 -> 3, 3 -> 4 -> 3, 5 alone.
        let adj = vec![vec![1], vec![2], vec![0, 3], vec![4], vec![3], vec![]];
        let mut sccs: Vec<Vec<usize>> = tarjan(&adj)
            .into_iter()
            .map(|mut c| {
                c.sort();
                c
            })
            .collect();
        sccs.sort();
        assert_eq!(sccs, vec![vec![0, 1, 2], vec![3, 4], vec![5]]);
    }
}
