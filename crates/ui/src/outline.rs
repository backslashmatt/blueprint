//! The outline tree (#117, REQUIREMENTS §5.5): the map's non-visual
//! equivalent. Every node of the review from L1 down, as an indented tree
//! that expands and collapses by key, each row with its change status and
//! its edges to its siblings. It is built from the map's own views, so a
//! node is in the tree exactly when the map can show it.

use std::collections::BTreeSet;

use blueprint_protocol::model::NodeId;

use crate::map::{EdgeStatus, Map, MapNode, Status};

/// One row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The node, as its map view shows it.
    pub node: MapNode,
    /// How deep it is: 0 for a system.
    pub depth: usize,
    /// Whether its children are shown.
    pub expanded: bool,
    /// Edges in head from it to its siblings: what it depends on, in its
    /// view. Edges only in base, now gone, are not counted.
    pub out: usize,
    /// Edges in head from its siblings to it: what depends on it.
    pub into: usize,
}

/// A key the tree acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// The row above.
    Up,
    /// The row below.
    Down,
    /// The first row.
    First,
    /// The last row.
    Last,
    /// Expand the row, or if it is expanded, go to its first child.
    Expand,
    /// Collapse the row, or if it is collapsed, go to its parent.
    Collapse,
}

/// The tree's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    /// The visible rows, in tree order.
    pub rows: Vec<Row>,
    /// The highlighted row.
    pub cursor: usize,
    expanded: BTreeSet<NodeId>,
}

impl Outline {
    /// The tree of `map`'s review, opened down to the map's current view
    /// with the map's selected node highlighted.
    #[must_use]
    pub fn new(map: &Map) -> Outline {
        let mut o = Outline {
            expanded: map.path.iter().cloned().collect(),
            ..Outline::default()
        };
        o.rebuild(map);
        let selected = map
            .selected
            .and_then(|i| map.view.nodes.get(i))
            .map(|n| n.id.clone());
        o.select(selected.as_ref());
        o
    }

    /// Rebuilds the rows from `map`, a newer map of the same review (the
    /// full one after the coarse one), keeping what is expanded and the
    /// highlighted node.
    pub fn refresh(&mut self, map: &Map) {
        let current = self.current().cloned();
        self.rebuild(map);
        self.select(current.as_ref());
    }

    /// The highlighted node.
    #[must_use]
    pub fn current(&self) -> Option<&NodeId> {
        self.rows.get(self.cursor).map(|r| &r.node.id)
    }

    /// Applies one key. Returns whether anything changed.
    pub fn apply(&mut self, map: &Map, op: Op) -> bool {
        let before = (self.cursor, self.rows.len());
        let last = self.rows.len().saturating_sub(1);
        match op {
            Op::Up => self.cursor = self.cursor.saturating_sub(1),
            Op::Down => self.cursor = (self.cursor + 1).min(last),
            Op::First => self.cursor = 0,
            Op::Last => self.cursor = last,
            Op::Expand => {
                let Some(row) = self.rows.get(self.cursor) else {
                    return false;
                };
                if !row.node.has_children {
                    return false;
                }
                if row.expanded {
                    self.cursor = (self.cursor + 1).min(last);
                } else {
                    let id = row.node.id.clone();
                    self.expanded.insert(id.clone());
                    self.rebuild(map);
                    self.select(Some(&id));
                }
            }
            Op::Collapse => {
                let Some(row) = self.rows.get(self.cursor) else {
                    return false;
                };
                if row.expanded {
                    let id = row.node.id.clone();
                    self.expanded.remove(&id);
                    self.rebuild(map);
                    self.select(Some(&id));
                } else if let Some(p) = self.parent_row(self.cursor) {
                    self.cursor = p;
                }
            }
        }
        (self.cursor, self.rows.len()) != before
    }

    /// The row of the nearest row above `i` that is one level shallower.
    fn parent_row(&self, i: usize) -> Option<usize> {
        let depth = self.rows.get(i)?.depth;
        (0..i).rev().find(|&j| self.rows[j].depth + 1 == depth)
    }

    /// Highlights `id`'s row, if it is shown; else keeps the cursor in range.
    fn select(&mut self, id: Option<&NodeId>) {
        if let Some(i) = id.and_then(|id| self.rows.iter().position(|r| &r.node.id == id)) {
            self.cursor = i;
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    fn rebuild(&mut self, map: &Map) {
        self.rows.clear();
        self.add(map, None, 0);
    }

    /// Adds the rows of the view under `parent`, and below each expanded
    /// one, its children's.
    fn add(&mut self, map: &Map, parent: Option<&NodeId>, depth: usize) {
        let view = map.view_of(parent);
        // One pass over the view's edges for every row's counts.
        let (mut outs, mut intos) = (vec![0; view.nodes.len()], vec![0; view.nodes.len()]);
        for e in view
            .edges
            .iter()
            .filter(|e| e.status != EdgeStatus::Removed)
        {
            outs[e.from] += 1;
            intos[e.to] += 1;
        }
        for (i, node) in view.nodes.into_iter().enumerate() {
            let (out, into) = (outs[i], intos[i]);
            let expanded = node.has_children && self.expanded.contains(&node.id);
            let id = node.id.clone();
            self.rows.push(Row {
                node,
                depth,
                expanded,
                out,
                into,
            });
            if expanded {
                self.add(map, Some(&id), depth + 1);
            }
        }
    }
}

/// A status in words, for a row (and a screen reader).
#[must_use]
pub fn status_word(s: Status) -> &'static str {
    match s {
        Status::Added => "added",
        Status::Removed => "removed",
        Status::Surface => "surface changed",
        Status::Moved => "moved",
        Status::Changed => "changed",
        Status::Contained => "body changed",
        Status::Inside => "changed inside",
        Status::Unchanged => "unchanged",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use blueprint_engine::analyse_deep;
    use blueprint_protocol::model::{Model, RepoPath};

    fn model(files: &[(&str, &str)]) -> Model {
        let paths: Vec<RepoPath> = files
            .iter()
            .map(|(p, _)| RepoPath::parse(p).unwrap())
            .collect();
        let read = |p: &RepoPath| {
            files
                .iter()
                .find(|(q, _)| *q == p.to_string())
                .map(|(_, t)| t.as_bytes().to_vec())
        };
        analyse_deep(&paths, &read).0
    }

    fn review() -> Map {
        let ws = ("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n");
        let base = model(&[
            ws,
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
            ws,
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            (
                "a/src/lib.rs",
                "pub mod x;\npub fn f() -> u64 {\n    1\n}\n",
            ),
            ("a/src/x.rs", "pub fn g() {\n    let _ = 1;\n}\n"),
            (
                "b/Cargo.toml",
                "[package]\nname = \"b\"\n[dependencies]\na = { path = \"../a\" }\n",
            ),
            ("b/src/lib.rs", "pub fn h() {\n    a::f();\n}\n"),
        ]);
        let diff = blueprint_engine::diff::diff(&base, &head);
        Map::new(Arc::new(base), Arc::new(head), &diff)
    }

    /// Every node the map can reach, by drilling everywhere.
    fn reachable(map: &Map) -> BTreeSet<NodeId> {
        let mut out = BTreeSet::new();
        let mut todo = vec![None];
        while let Some(parent) = todo.pop() {
            for n in map.view_of(parent.as_ref()).nodes {
                if n.has_children {
                    todo.push(Some(n.id.clone()));
                }
                out.insert(n.id);
            }
        }
        out
    }

    #[test]
    fn every_node_on_the_map_is_in_the_tree() {
        let map = review();
        let mut o = Outline::new(&map);
        // Expand everything, row by row.
        let mut i = 0;
        while i < o.rows.len() {
            o.cursor = i;
            if o.rows[i].node.has_children && !o.rows[i].expanded {
                o.apply(&map, Op::Expand);
            }
            i += 1;
        }
        let rows: BTreeSet<NodeId> = o.rows.iter().map(|r| r.node.id.clone()).collect();
        assert_eq!(rows, reachable(&map));
        assert!(o.rows.iter().any(|r| r.depth == 3), "down to L4");
    }

    #[test]
    fn the_selection_survives_switching_both_ways() {
        let mut map = review();
        // On the map: drill into the system with the change, select a node.
        let a = map.view.nodes.iter().position(|n| n.name == "a");
        map.selected = a;
        assert!(map.drill_down());
        let chosen = map.view.nodes[map.selected.unwrap()].id.clone();

        let mut o = Outline::new(&map);
        assert_eq!(o.current(), Some(&chosen), "map to tree");

        // In the tree: move to another node, then back to the map.
        o.apply(&map, Op::Collapse);
        o.apply(&map, Op::Collapse);
        let other = o.current().cloned().unwrap();
        assert_ne!(other, chosen);
        assert!(map.reveal(&other));
        assert_eq!(
            map.selected.map(|i| map.view.nodes[i].id.clone()),
            Some(other),
            "tree to map"
        );
    }

    #[test]
    fn arrows_expand_collapse_and_move() {
        let map = review();
        let mut o = Outline::new(&map);
        let systems = o.rows.len();
        o.apply(&map, Op::First);
        let a = o.rows.iter().position(|r| r.node.name == "a").unwrap();
        o.cursor = a;
        assert!(o.apply(&map, Op::Expand), "expands");
        assert!(o.rows.len() > systems);
        assert!(o.rows[a].expanded);
        assert!(o.apply(&map, Op::Expand), "then moves to the first child");
        assert_eq!(o.cursor, a + 1);
        assert_eq!(o.rows[o.cursor].depth, 1);
        assert!(
            o.apply(&map, Op::Collapse),
            "a collapsed child goes to its parent"
        );
        assert_eq!(o.cursor, a);
        assert!(o.apply(&map, Op::Collapse), "then collapses");
        assert_eq!(o.rows.len(), systems);
        o.apply(&map, Op::Last);
        assert_eq!(o.cursor, systems - 1);
        assert!(!o.apply(&map, Op::Down), "stays in the tree");
    }

    #[test]
    fn a_newer_map_refreshes_the_rows_and_keeps_the_place() {
        let full = review();
        // The coarse map: L1–L2 only, so modules have nothing inside.
        let coarse = {
            let strip = |m: &Model| {
                let keep: Vec<_> = m
                    .nodes()
                    .filter(|n| n.id.level() <= blueprint_protocol::model::Level::Module)
                    .cloned()
                    .collect();
                Model::from_trusted_parts(keep, Vec::new())
            };
            let (b, h) = (strip(full.base()), strip(full.head()));
            let diff = blueprint_engine::diff::diff(&b, &h);
            Map::new(Arc::new(b), Arc::new(h), &diff)
        };
        let mut o = Outline::new(&coarse);
        o.cursor = o.rows.iter().position(|r| r.node.name == "a").unwrap();
        o.apply(&coarse, Op::Expand);
        o.apply(&coarse, Op::Expand);
        let module = o.current().cloned().unwrap();
        assert!(
            !o.rows[o.cursor].node.has_children,
            "nothing inside, coarse"
        );

        o.refresh(&full);
        assert_eq!(o.current(), Some(&module), "the same row");
        assert!(
            o.rows[o.cursor].node.has_children,
            "the full map has its items"
        );
        assert!(o.apply(&full, Op::Expand));
    }

    #[test]
    fn rows_count_their_edges_to_siblings() {
        let map = review();
        let o = Outline::new(&map);
        let row = |name: &str| o.rows.iter().find(|r| r.node.name == name).unwrap();
        // b now depends on a.
        assert!(row("b").out >= 1, "{:?}", row("b"));
        assert!(row("a").into >= 1, "{:?}", row("a"));
    }
}
