//! The map (REQUIREMENTS §5.4, view 2; #92): one level of the architecture
//! at a time, base and head together, with the diff overlaid. View state
//! only, so it is tested headlessly; `workspace` lays it out (DR 0010) and
//! draws it.
//!
//! A view is the children of one node (or the systems, at the top). It
//! shows the union of base and head, so that switching between them never
//! moves anything (DR 0010), and every model edge rolled up to the pair of
//! siblings whose subtrees it joins (§3.2).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use blueprint_protocol::model::diff::{Diff, NodeChange};
use blueprint_protocol::model::{Model, NodeId, NodeKind};

/// How a node changed, as the overlay shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Only in head.
    Added,
    /// Only in base, or the old place of a moved or renamed node.
    Removed,
    /// Its public surface changed.
    Surface,
    /// Moved or renamed here.
    Moved,
    /// Changed in a way that adds or removes edges, or privately.
    Changed,
    /// Only its body changed: the quiet state (§3.3).
    Contained,
    /// Unchanged itself, but something inside it changed.
    Inside,
    /// Nothing in it changed.
    Unchanged,
}

/// How an edge changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeStatus {
    /// Only in head: some edge under it is new.
    Added,
    /// Only in base: every edge under it is gone.
    Removed,
    /// In both.
    Unchanged,
}

/// One node of a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapNode {
    /// Its id.
    pub id: NodeId,
    /// Its name.
    pub name: String,
    /// What it is.
    pub kind: NodeKind,
    /// How it changed.
    pub status: Status,
    /// Whether it has children in base or head, so drilling into it shows
    /// something.
    pub has_children: bool,
}

/// One rolled-up edge of a view, between two of its nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MapEdge {
    /// The dependent node's index in [`View::nodes`].
    pub from: usize,
    /// The node it depends on.
    pub to: usize,
    /// How it changed.
    pub status: EdgeStatus,
}

/// What one view shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct View {
    /// Whose children these are; `None` for the systems.
    pub parent: Option<NodeId>,
    /// The nodes, sorted by id.
    pub nodes: Vec<MapNode>,
    /// The edges, sorted.
    pub edges: Vec<MapEdge>,
}

impl View {
    /// The ids, in node order, as the layout takes them.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.nodes.iter().map(|n| n.id.to_string()).collect()
    }

    /// The edges as `(from, to)` index pairs, as the layout takes them.
    #[must_use]
    pub fn pairs(&self) -> Vec<(usize, usize)> {
        self.edges.iter().map(|e| (e.from, e.to)).collect()
    }
}

/// A review's map: both models, the diff, where the reviewer is and what is
/// selected.
#[derive(Debug, Clone)]
pub struct Map {
    base: Arc<Model>,
    head: Arc<Model>,
    status: HashMap<NodeId, Status>,
    /// The nodes drilled into, from the top: the breadcrumb.
    pub path: Vec<NodeId>,
    /// The current view.
    pub view: View,
    /// The selected node's index in the view, if any.
    pub selected: Option<usize>,
}

impl Map {
    /// The map of a diff, at the systems.
    #[must_use]
    pub fn new(base: Arc<Model>, head: Arc<Model>, diff: &Diff) -> Map {
        let mut status = HashMap::new();
        for change in &diff.nodes {
            let (id, s) = match change {
                NodeChange::Added { id } => (id, Status::Added),
                NodeChange::Removed { id } => (id, Status::Removed),
                NodeChange::Surface { id } => (id, Status::Surface),
                NodeChange::Contained { id } => (id, Status::Contained),
                NodeChange::Changed { id } => (id, Status::Changed),
                NodeChange::Moved { from, to } | NodeChange::Renamed { from, to } => {
                    status.insert(from.clone(), Status::Removed);
                    (to, Status::Moved)
                }
            };
            status.insert(id.clone(), s);
        }
        // Everything above a change, in either model, has changes inside.
        let mut inside = Vec::new();
        for id in status.keys() {
            for model in [&base, &head] {
                let mut at = model.node(id).and_then(|n| n.parent.clone());
                while let Some(p) = at {
                    at = model.node(&p).and_then(|n| n.parent.clone());
                    inside.push(p);
                }
            }
        }
        for p in inside {
            status.entry(p).or_insert(Status::Inside);
        }
        let mut map = Map {
            base,
            head,
            status,
            path: Vec::new(),
            view: View::default(),
            selected: None,
        };
        map.rebuild();
        map
    }

    fn rebuild(&mut self) {
        self.view = view(&self.base, &self.head, &self.status, self.path.last());
        // Select the most changed node, so Enter goes where the change is.
        self.selected = self
            .view
            .nodes
            .iter()
            .enumerate()
            .min_by_key(|(_, n)| n.status)
            .map(|(i, _)| i);
    }

    /// Drills into the selected node. Returns whether the view changed.
    pub fn drill_down(&mut self) -> bool {
        let Some(node) = self.selected.and_then(|i| self.view.nodes.get(i)) else {
            return false;
        };
        if !node.has_children {
            return false;
        }
        self.path.push(node.id.clone());
        self.rebuild();
        true
    }

    /// Goes up one level, selecting the node just left. Returns whether the
    /// view changed.
    pub fn drill_up(&mut self) -> bool {
        let Some(left) = self.path.pop() else {
            return false;
        };
        self.rebuild();
        self.selected = self.view.nodes.iter().position(|n| n.id == left);
        true
    }

    /// Selects the next node (`forward`) or the previous one, in id order.
    pub fn cycle(&mut self, forward: bool) {
        let k = self.view.nodes.len();
        if k == 0 {
            return;
        }
        self.selected = Some(match (self.selected, forward) {
            (None, _) => 0,
            (Some(i), true) => (i + 1) % k,
            (Some(i), false) => (i + k - 1) % k,
        });
    }

    /// The breadcrumb: the names of the nodes drilled into.
    #[must_use]
    pub fn breadcrumb(&self) -> Vec<String> {
        self.path
            .iter()
            .map(|id| {
                self.head
                    .node(id)
                    .or_else(|| self.base.node(id))
                    .map_or_else(|| id.to_string(), |n| n.name.clone())
            })
            .collect()
    }
}

/// The point nearest `from` in the direction `(dx, dy)` (one of them zero,
/// the other ±1): among the points ahead of it, the one with the smallest
/// distance ahead plus twice the distance aside.
#[must_use]
pub fn nearest_towards(points: &[(f32, f32)], from: usize, dx: f32, dy: f32) -> Option<usize> {
    let (fx, fy) = *points.get(from)?;
    points
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != from)
        .filter_map(|(i, (x, y))| {
            let ahead = (x - fx) * dx + (y - fy) * dy;
            let aside = ((x - fx) * dy).abs() + ((y - fy) * dx).abs();
            (ahead > 0.5).then_some((ahead + 2.0 * aside, i))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, i)| i)
}

/// The view under `parent` (`None`: the systems) of base and head together.
fn view(
    base: &Model,
    head: &Model,
    status: &HashMap<NodeId, Status>,
    parent: Option<&NodeId>,
) -> View {
    let children = |m: &Model| -> Vec<NodeId> {
        match parent {
            Some(p) => m.children(p).map(|n| n.id.clone()).collect(),
            None => m
                .nodes()
                .filter(|n| n.parent.is_none())
                .map(|n| n.id.clone())
                .collect(),
        }
    };
    let ids: BTreeSet<NodeId> = children(base).into_iter().chain(children(head)).collect();
    let nodes: Vec<MapNode> = ids
        .iter()
        .map(|id| {
            let node = head.node(id).or_else(|| base.node(id));
            let has_children = [base, head].iter().any(|m| m.children(id).next().is_some());
            MapNode {
                id: id.clone(),
                name: node.map_or_else(|| id.to_string(), |n| n.name.clone()),
                kind: node.map_or(NodeKind::Directory, |n| n.kind),
                status: status.get(id).copied().unwrap_or(Status::Unchanged),
                has_children,
            }
        })
        .collect();
    let index: BTreeMap<&NodeId, usize> = ids.iter().enumerate().map(|(i, id)| (id, i)).collect();
    // Roll each model's edges up to this view's nodes.
    let rolled = |m: &Model| -> BTreeSet<(usize, usize)> {
        let lift = |id: &NodeId| -> Option<usize> {
            let mut at = id.clone();
            loop {
                let node = m.node(&at)?;
                if node.parent.as_ref() == parent {
                    return index.get(&at).copied();
                }
                at = node.parent.clone()?;
            }
        };
        m.edges()
            .filter_map(|e| Some((lift(&e.key.from)?, lift(&e.key.to)?)))
            .filter(|(a, b)| a != b)
            .collect()
    };
    let (before, after) = (rolled(base), rolled(head));
    let edges = before
        .union(&after)
        .map(|&(from, to)| MapEdge {
            from,
            to,
            status: match (before.contains(&(from, to)), after.contains(&(from, to))) {
                (false, true) => EdgeStatus::Added,
                (true, false) => EdgeStatus::Removed,
                _ => EdgeStatus::Unchanged,
            },
        })
        .collect();
    View {
        parent: parent.cloned(),
        nodes,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blueprint_engine::analyse_deep;
    use blueprint_protocol::model::RepoPath;

    fn model(files: &[(&str, &str)]) -> Model {
        let map: BTreeMap<RepoPath, Vec<u8>> = files
            .iter()
            .filter_map(|(p, b)| Some((RepoPath::parse(p).ok()?, b.as_bytes().to_vec())))
            .collect();
        let paths: Vec<RepoPath> = map.keys().cloned().collect();
        analyse_deep(&paths, &|p| map.get(p).cloned()).0
    }

    const WS: (&str, &str) = ("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n");

    fn review() -> Map {
        let base = model(&[
            WS,
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            (
                "a/src/lib.rs",
                "pub mod x;\npub fn f() -> u32 {\n    1\n}\n",
            ),
            ("a/src/x.rs", "pub fn g() {}\n"),
            ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            ("b/src/lib.rs", "pub fn h() {}\n"),
        ]);
        let head = model(&[
            WS,
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            (
                "a/src/lib.rs",
                "pub mod x;\npub fn f() -> u32 {\n    2\n}\n",
            ),
            ("a/src/x.rs", "pub fn g() {}\n"),
            (
                "b/Cargo.toml",
                "[package]\nname = \"b\"\n\n[dependencies]\na = { path = \"../a\" }\n",
            ),
            ("b/src/lib.rs", "pub fn h() {\n    a::f();\n}\n"),
        ]);
        let diff = blueprint_engine::diff::diff(&base, &head);
        Map::new(Arc::new(base), Arc::new(head), &diff)
    }

    fn named<'a>(v: &'a View, name: &str) -> Option<&'a MapNode> {
        v.nodes.iter().find(|n| n.name == name)
    }

    #[test]
    fn the_top_view_shows_systems_with_the_new_dependency() {
        let m = review();
        let a = named(&m.view, "a").map(|n| n.status);
        assert_eq!(a, Some(Status::Inside), "f's body changed inside a");
        let (ia, ib) = (
            m.view.nodes.iter().position(|n| n.name == "a"),
            m.view.nodes.iter().position(|n| n.name == "b"),
        );
        let new: Vec<&MapEdge> = m
            .view
            .edges
            .iter()
            .filter(|e| e.status == EdgeStatus::Added)
            .collect();
        assert!(
            new.iter().any(|e| Some(e.from) == ib && Some(e.to) == ia),
            "b -> a is new: {:?}",
            m.view.edges
        );
        assert_eq!(m.view.ids().len(), m.view.nodes.len());
    }

    #[test]
    fn drilling_down_and_up_keeps_a_breadcrumb_and_the_selection() {
        let mut m = review();
        let a = m.view.nodes.iter().position(|n| n.name == "a");
        m.selected = a;
        assert!(m.drill_down());
        assert_eq!(m.breadcrumb(), vec!["a".to_string()]);
        assert!(!m.view.nodes.is_empty());
        // Down to the functions. f's body changed and b now calls it, so it
        // is changed, not contained (an edge to it is new).
        let mut found = None;
        for _ in 0..4 {
            if let Some(f) = named(&m.view, "f") {
                found = Some(f.status);
                break;
            }
            m.selected = m.view.nodes.iter().position(|n| n.has_children);
            if !m.drill_down() {
                break;
            }
        }
        assert_eq!(found, Some(Status::Changed), "{:?}", m.view.nodes);
        while m.drill_up() {}
        assert!(m.path.is_empty());
        assert_eq!(m.selected, a, "back at the top with a selected");
        assert!(!m.drill_up());
    }

    #[test]
    fn arrows_pick_the_nearest_node_in_their_direction() {
        let pts = [
            (0.0, 0.0),
            (100.0, 0.0),
            (300.0, 0.0),
            (0.0, 100.0),
            (90.0, 120.0),
        ];
        assert_eq!(nearest_towards(&pts, 0, 1.0, 0.0), Some(1));
        assert_eq!(nearest_towards(&pts, 1, 1.0, 0.0), Some(2));
        assert_eq!(nearest_towards(&pts, 0, 0.0, 1.0), Some(3));
        assert_eq!(nearest_towards(&pts, 1, 0.0, 1.0), Some(4));
        assert_eq!(nearest_towards(&pts, 0, -1.0, 0.0), None);
        assert_eq!(nearest_towards(&pts, 9, 1.0, 0.0), None);
    }

    #[test]
    fn selection_cycles_and_leaves_cannot_be_drilled() {
        let mut m = review();
        let k = m.view.nodes.len();
        let start = m.selected;
        for _ in 0..k {
            m.cycle(true);
        }
        assert_eq!(m.selected, start);
        m.cycle(false);
        m.cycle(true);
        assert_eq!(m.selected, start);
        let mut empty = Map::new(Arc::default(), Arc::default(), &Diff::default());
        assert_eq!(empty.selected, None);
        assert!(!empty.drill_down());
        empty.cycle(true);
        assert_eq!(empty.selected, None);
    }
}
