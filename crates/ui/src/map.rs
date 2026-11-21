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
    /// The file or directory that defines it, `/`-separated.
    pub source: String,
    /// Its lines in that file, 1-based and inclusive, for code nodes (in
    /// head, or in base for a node only there: see `lines_in_base`).
    pub lines: Option<(u32, u32)>,
    /// Whether `lines` count base's lines: the node is not in head.
    pub lines_in_base: bool,
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
    /// Every model edge rolled up once: per view (its parent), each pair of
    /// siblings joined, and whether base and head have it. Drilling then
    /// costs only the view's own edges.
    rolled: Arc<Rolled>,
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
        // Everything above a change, in either model, has changes inside,
        // except above a contained change: that stays quiet (§3.3).
        let mut inside = Vec::new();
        for (id, s) in &status {
            if *s == Status::Contained {
                continue;
            }
            for model in [&base, &head] {
                inside.extend(chain(model, id).into_iter().flatten().skip(1));
            }
        }
        for p in inside {
            status.entry(p).or_insert(Status::Inside);
        }
        let rolled = Arc::new(roll_up(&base, &head));
        let mut map = Map {
            base,
            head,
            status,
            rolled,
            path: Vec::new(),
            view: View::default(),
            selected: None,
        };
        map.rebuild();
        map
    }

    fn rebuild(&mut self) {
        self.view = view(
            &self.base,
            &self.head,
            &self.status,
            &self.rolled,
            self.path.last(),
        );
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

    /// Shows `id` in its view, drilled down to its parent, and selects it.
    /// Returns whether the node was found (in head, else in base).
    pub fn reveal(&mut self, id: &NodeId) -> bool {
        let Some(chain) = chain(&self.head, id).or_else(|| chain(&self.base, id)) else {
            return false;
        };
        // The chain runs from the node up; the path runs down to its parent.
        self.path = chain.iter().skip(1).rev().cloned().collect();
        self.rebuild();
        self.selected = self.view.nodes.iter().position(|n| &n.id == id);
        self.selected.is_some()
    }

    /// A node's parent, from head or else base.
    #[must_use]
    pub fn parent(&self, id: &NodeId) -> Option<NodeId> {
        self.head
            .node(id)
            .or_else(|| self.base.node(id))
            .and_then(|n| n.parent.clone())
    }

    /// A node's name, from head or else base; its id if neither has it.
    #[must_use]
    pub fn name(&self, id: &NodeId) -> String {
        self.head
            .node(id)
            .or_else(|| self.base.node(id))
            .map_or_else(|| id.to_string(), |n| n.name.clone())
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

/// The deepest a node may be. Models are trees, but one loaded from a
/// damaged index is not checked; a parent cycle must not hang the map.
const MAX_DEPTH: usize = 256;

/// `id` and its ancestors, nearest first; `None` if the chain is broken off
/// by a cycle or is deeper than [`MAX_DEPTH`].
fn chain(m: &Model, id: &NodeId) -> Option<Vec<NodeId>> {
    let mut out = vec![id.clone()];
    let mut at = m.node(id)?.parent.clone();
    while let Some(p) = at {
        if out.len() > MAX_DEPTH {
            return None;
        }
        at = m.node(&p).and_then(|n| n.parent.clone());
        out.push(p);
    }
    Some(out)
}

/// Per view (its parent), each pair of siblings an edge joins: whether base
/// has it, and whether head does.
type Rolled = HashMap<Option<NodeId>, BTreeMap<(NodeId, NodeId), (bool, bool)>>;

/// Rolls every edge of both models up to the view where its ends part: the
/// children of their nearest common ancestor (§3.2).
fn roll_up(base: &Model, head: &Model) -> Rolled {
    let mut out = Rolled::new();
    for (in_head, m) in [(false, base), (true, head)] {
        for e in m.edges() {
            let (Some(mut a), Some(mut b)) = (chain(m, &e.key.from), chain(m, &e.key.to)) else {
                continue;
            };
            a.reverse();
            b.reverse();
            let Some(i) = (0..a.len().min(b.len())).find(|i| a[*i] != b[*i]) else {
                continue;
            };
            let parent = if i == 0 { None } else { Some(a[i - 1].clone()) };
            let flags = out
                .entry(parent)
                .or_default()
                .entry((a[i].clone(), b[i].clone()))
                .or_default();
            if in_head {
                flags.1 = true;
            } else {
                flags.0 = true;
            }
        }
    }
    out
}

/// The view under `parent` (`None`: the systems) of base and head together.
fn view(
    base: &Model,
    head: &Model,
    status: &HashMap<NodeId, Status>,
    rolled: &Rolled,
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
                source: node.map(|n| n.source.to_string()).unwrap_or_default(),
                lines: node.and_then(|n| n.code).map(|c| c.lines),
                lines_in_base: head.node(id).is_none(),
            }
        })
        .collect();
    let index: BTreeMap<&NodeId, usize> = ids.iter().enumerate().map(|(i, id)| (id, i)).collect();
    let mut edges: Vec<MapEdge> = rolled
        .get(&parent.cloned())
        .into_iter()
        .flatten()
        .filter_map(|((a, b), (before, after))| {
            Some(MapEdge {
                from: *index.get(a)?,
                to: *index.get(b)?,
                status: match (before, after) {
                    (false, true) => EdgeStatus::Added,
                    (true, false) => EdgeStatus::Removed,
                    _ => EdgeStatus::Unchanged,
                },
            })
        })
        .collect();
    edges.sort();
    View {
        parent: parent.cloned(),
        nodes,
        edges,
    }
}
/// How the map view is looked at (#92): the selection is centred, moved
/// by `pan` (screen pixels) and scaled by `zoom`, which is a whole number of
/// [`Camera::ZOOM_STEP`]s from 100%, so zooming back always meets 100%.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// Offset from centring the selection, in screen pixels.
    pub pan: (f32, f32),
    /// Scale: [`Camera::ZOOM_STEP`] to the power `steps`.
    pub zoom: f32,
    /// Zoom steps from 100%, within [`Camera::MIN_STEPS`] and
    /// [`Camera::MAX_STEPS`].
    pub steps: i32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            pan: (0.0, 0.0),
            zoom: 1.0,
            steps: 0,
        }
    }
}

impl Camera {
    /// The fewest steps: 1.25^-6, about 26%.
    pub const MIN_STEPS: i32 = -6;
    /// The most steps: 1.25^6, about 381%.
    pub const MAX_STEPS: i32 = 6;
    /// One pan step, in screen pixels.
    pub const PAN_STEP: f32 = 120.0;
    /// One zoom step, as a factor.
    pub const ZOOM_STEP: f32 = 1.25;

    /// Moves the view by one step in direction `(dx, dy)` (each -1, 0 or
    /// 1): the content moves the other way, as when panning a document.
    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.pan.0 -= dx * Self::PAN_STEP;
        self.pan.1 -= dy * Self::PAN_STEP;
    }

    /// Zooms `steps` steps in (`steps > 0`) or out, about the screen
    /// centre, so that what is in the middle stays there.
    pub fn zoom_by(&mut self, steps: i32) {
        self.steps = (self.steps + steps).clamp(Self::MIN_STEPS, Self::MAX_STEPS);
        let to = Self::ZOOM_STEP.powi(self.steps);
        let k = to / self.zoom;
        self.pan = (self.pan.0 * k, self.pan.1 * k);
        self.zoom = to;
    }

    /// Where a point of the layout lands on screen, for a viewport of
    /// `viewport` with `centre` (a layout point) brought to its middle.
    #[must_use]
    pub fn to_screen(&self, p: (f32, f32), centre: (f32, f32), viewport: (f32, f32)) -> (f32, f32) {
        (
            (p.0 - centre.0) * self.zoom + viewport.0 / 2.0 + self.pan.0,
            (p.1 - centre.1) * self.zoom + viewport.1 / 2.0 + self.pan.1,
        )
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
    fn a_contained_change_stays_quiet_above_it() {
        let base = model(&[
            ("Cargo.toml", "[package]\nname = \"q\"\n"),
            ("src/lib.rs", "pub fn f() -> u32 {\n    1\n}\n"),
        ]);
        let head = model(&[
            ("Cargo.toml", "[package]\nname = \"q\"\n"),
            ("src/lib.rs", "pub fn f() -> u32 {\n    2\n}\n"),
        ]);
        let diff = blueprint_engine::diff::diff(&base, &head);
        let m = Map::new(Arc::new(base), Arc::new(head), &diff);
        assert!(
            m.view.nodes.iter().all(|n| n.status == Status::Unchanged),
            "{:?}",
            m.view.nodes
        );
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

    #[test]
    fn reveal_drills_to_a_nodes_parent_and_selects_it() {
        let mut m = review();
        let id = m
            .view
            .nodes
            .iter()
            .find(|n| n.name == "a")
            .map(|n| n.id.clone());
        let Some(a) = id else {
            panic!("a is a system");
        };
        m.selected = m.view.nodes.iter().position(|n| n.id == a);
        assert!(m.drill_down());
        let inner = m.view.nodes.first().map(|n| n.id.clone());
        let Some(inner) = inner else {
            panic!("a has children");
        };
        m.drill_up();
        assert!(m.path.is_empty());
        assert!(m.reveal(&inner));
        assert_eq!(m.path, vec![a.clone()]);
        assert_eq!(
            m.selected.and_then(|i| m.view.nodes.get(i)).map(|n| &n.id),
            Some(&inner)
        );
        assert_eq!(m.name(&a), "a");
    }

    #[test]
    fn the_camera_pans_and_zooms_about_the_centre() {
        let mut c = Camera::default();
        let (centre, vp) = ((100.0, 50.0), (800.0, 600.0));
        assert_eq!(c.to_screen(centre, centre, vp), (400.0, 300.0));
        c.pan_by(1.0, 0.0);
        assert_eq!(
            c.to_screen(centre, centre, vp),
            (280.0, 300.0),
            "panning right moves content left"
        );
        let middle_before = c.to_screen((220.0, 50.0), centre, vp);
        c.zoom_by(1);
        let middle_after = c.to_screen((220.0, 50.0), centre, vp);
        assert!((middle_before.0 - 400.0).abs() < 1e-3 && (middle_after.0 - 400.0).abs() < 1e-3);
        for _ in 0..40 {
            c.zoom_by(1);
        }
        assert_eq!(c.steps, Camera::MAX_STEPS);
        // Back from the limit, the steps meet 100% again.
        c.zoom_by(-Camera::MAX_STEPS);
        assert!((c.zoom - 1.0).abs() < 1e-6, "{}", c.zoom);
        for _ in 0..80 {
            c.zoom_by(-1);
        }
        assert_eq!(c.steps, Camera::MIN_STEPS);
    }
}
