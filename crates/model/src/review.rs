//! Review state (REQUIREMENTS §5.4 view 5, DR 0011): which changed nodes a
//! reviewer has marked reviewed, how that rolls up, and the key a mark is
//! stored under so that it survives a rebase that leaves the node's change as
//! it was. No I/O: the engine stores [`Mark`]s, front ends compute
//! [`ReviewState`] from them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::diff::{Diff, Lines, NodeChange};
use crate::findings::{self, Item};
use crate::{Model, Node, NodeId};

/// Bumped whenever [`fingerprints`] or [`ReviewState::key`] would give a
/// different value for the same models. Marks stored under older keys then
/// stop matching, which clears them: it never applies one to the wrong change.
pub const KEY_VERSION: u8 = 1;

/// A reviewer's mark on one node of one change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// The node, as in head (as in base for a removed node).
    pub node: NodeId,
    /// [`ReviewState::key`] of the node when it was marked.
    pub key: u64,
    /// Whether the reviewer marked it reviewed.
    pub reviewed: bool,
    /// The reviewer's note, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The content of each node and everything under it, as a hash: kind, name,
/// source, visibility, confidence and code hashes, but not line numbers, so
/// code that only moved within its file keeps its fingerprint.
#[must_use]
pub fn fingerprints(model: &Model) -> HashMap<NodeId, u64> {
    let mut out: HashMap<NodeId, u64> = HashMap::new();
    // Ids sort by level first, so in reverse every child comes before its parent.
    let nodes: Vec<&Node> = model.nodes().collect();
    for node in nodes.into_iter().rev() {
        let mut h = Fnv::new();
        h.str(&node.id.to_string());
        h.str(&format!("{:?}", node.kind));
        h.str(&node.name);
        h.str(&node.source.segments().join("/"));
        h.str(&format!("{:?}{:?}", node.visibility, node.confidence));
        if let Some(code) = &node.code {
            h.u64(code.signature_hash);
            h.u64(u64::from(code.body_hash.is_some()));
            h.u64(code.body_hash.unwrap_or(0));
        }
        for child in model.children(&node.id) {
            h.u64(out.get(&child.id).copied().unwrap_or(0));
        }
        out.insert(node.id.clone(), h.finish());
    }
    out
}

/// Which changed nodes are reviewed, from the marks a reviewer made.
///
/// A node is reviewed when it, or a node above it, is marked reviewed:
/// reviewing a module reviews what is in it. It is also reviewed when every changed
/// child is reviewed and nothing in the node itself changed apart from its
/// children: no lines of its own (its weight less its children's), and no
/// move, rename or surface change of its own. A changed node with no changed
/// children is reviewed only by its own mark.
///
/// Built in two steps: a [`ReviewBasis`] once per diff, which reads both
/// models, then [`ReviewBasis::state`] for each set of marks, which only
/// visits changed nodes, so a front end can apply a mark at once.
#[derive(Debug, Clone, Default)]
pub struct ReviewState {
    basis: Arc<ReviewBasis>,
    marks: HashMap<NodeId, Mark>,
    reviewed: BTreeSet<NodeId>,
}

/// What a review state depends on besides the marks: each changed node's
/// key, its changed children, and whether only its own mark can make it
/// reviewed.
#[derive(Debug, Clone, Default)]
pub struct ReviewBasis {
    keys: HashMap<NodeId, u64>,
    /// Each changed node and ancestor of one, with its changed children.
    changed: BTreeMap<NodeId, Vec<NodeId>>,
    own_mark: BTreeSet<NodeId>,
}

impl ReviewState {
    /// The state of `diff` between `base` and `head`, given every mark
    /// stored for the repository: [`ReviewBasis::new`], then
    /// [`ReviewBasis::state`].
    #[must_use]
    pub fn new<'a>(
        diff: &Diff,
        base: &Model,
        head: &Model,
        marks: impl IntoIterator<Item = &'a Mark>,
    ) -> ReviewState {
        Arc::new(ReviewBasis::new(diff, base, head)).state(marks)
    }

    /// What this state was computed from.
    #[must_use]
    pub fn basis(&self) -> &Arc<ReviewBasis> {
        &self.basis
    }
}

impl ReviewBasis {
    /// The basis of `diff` between `base` and `head`: one pass over each
    /// model.
    #[must_use]
    pub fn new(diff: &Diff, base: &Model, head: &Model) -> ReviewBasis {
        let (base_fp, head_fp) = (fingerprints(base), fingerprints(head));
        // Each changed node's base id, for the nodes whose id changed.
        let mut base_id: HashMap<NodeId, NodeId> = HashMap::new();
        let mut own_change: HashSet<NodeId> = HashSet::new();
        let mut seeds: Vec<NodeId> = Vec::new();
        for change in &diff.nodes {
            match change {
                NodeChange::Moved { from, to } | NodeChange::Renamed { from, to } => {
                    base_id.insert(to.clone(), from.clone());
                    own_change.insert(to.clone());
                    seeds.push(to.clone());
                }
                NodeChange::Surface { id } => {
                    own_change.insert(id.clone());
                    seeds.push(id.clone());
                }
                NodeChange::Added { id }
                | NodeChange::Removed { id }
                | NodeChange::Contained { id }
                | NodeChange::Changed { id } => seeds.push(id.clone()),
            }
        }
        // A moved or renamed node's base id carries its removed lines: it is
        // the same node as its head id, which is the one reviewed.
        let head_id: HashMap<&NodeId, &NodeId> = base_id.iter().map(|(t, f)| (f, t)).collect();
        let status = |id: &NodeId| -> NodeId { head_id.get(id).map_or(id, |t| *t).clone() };
        let mut seeds: Vec<(NodeId, NodeId)> = seeds.into_iter().map(|s| (s.clone(), s)).collect();
        seeds.extend(
            diff.weights
                .iter()
                .filter(|w| !w.lines.is_zero())
                .map(|w| (status(&w.id), w.id.clone())),
        );

        let parent = |id: &NodeId| -> Option<NodeId> {
            head.node(id)
                .or_else(|| base.node(base_id.get(id).unwrap_or(id)))
                .and_then(|n| n.parent.clone())
        };
        // Every changed node and its ancestors, with their changed children
        // as (child, the id the parent's weight counted the child under).
        let mut changed: BTreeMap<NodeId, BTreeSet<(NodeId, NodeId)>> = BTreeMap::new();
        for (seed, weight_id) in seeds {
            changed.entry(seed.clone()).or_default();
            let mut child = (seed, weight_id);
            // Each ancestor under the id it is reviewed by: a moved
            // parent's base id is its head id.
            while let Some(p) = parent(&child.1) {
                let fresh = changed.entry(status(&p)).or_default().insert(child);
                if !fresh {
                    break;
                }
                child = (status(&p), p);
            }
        }

        // Own lines: each node's weight less its changed children's.
        let weight: HashMap<&NodeId, (Lines, u64)> = diff
            .weights
            .iter()
            .map(|w| (&w.id, (w.lines, w.digest)))
            .collect();
        let zero = Lines {
            signature: 0,
            body: 0,
        };
        let lines_of = |id: &NodeId| weight.get(id).map_or(zero, |w| w.0);
        let own_lines = |id: &NodeId, children: &BTreeSet<(NodeId, NodeId)>| -> bool {
            // A moved node's weight on both sides: its children were counted
            // under the base id on one side and the head id on the other.
            let mut own = lines_of(id);
            if let Some(from) = base_id.get(id) {
                own.add(lines_of(from));
            }
            for (_, c) in children {
                let l = lines_of(c);
                own.signature = own.signature.saturating_sub(l.signature);
                own.body = own.body.saturating_sub(l.body);
            }
            !own.is_zero()
        };

        let mut keys = HashMap::new();
        for id in changed.keys() {
            let b = base_id.get(id).unwrap_or(id);
            // What changed: the text of the node's changed lines on both
            // sides, for nodes whose content the fingerprints do not cover
            // (a file no analyser reads, a module's own lines).
            let digest = |i: &NodeId| weight.get(i).map_or(0, |w| w.1);
            let mut h = Fnv::new();
            h.u64(u64::from(KEY_VERSION));
            h.str(&id.to_string());
            h.u64(base_fp.get(b).copied().unwrap_or(0));
            h.u64(head_fp.get(id).copied().unwrap_or(0));
            h.u64(digest(id));
            h.u64(if b == id { 0 } else { digest(b) });
            keys.insert(id.clone(), h.finish());
        }
        let mut own_mark = BTreeSet::new();
        for (id, children) in &changed {
            if children.is_empty() || own_change.contains(id) || own_lines(id, children) {
                own_mark.insert(id.clone());
            }
        }
        let changed = changed
            .into_iter()
            .map(|(id, children)| {
                let mut c: Vec<NodeId> = children.into_iter().map(|(c, _)| c).collect();
                c.dedup();
                (id, c)
            })
            .collect();
        ReviewBasis {
            keys,
            changed,
            own_mark,
        }
    }

    /// The state given every mark stored for the repository. Marks whose
    /// key no longer matches, because the node's change is not the one that
    /// was marked, are ignored.
    #[must_use]
    pub fn state<'a>(self: &Arc<Self>, marks: impl IntoIterator<Item = &'a Mark>) -> ReviewState {
        let marks: HashMap<NodeId, Mark> = marks
            .into_iter()
            .filter(|m| self.keys.get(&m.node) == Some(&m.key))
            .map(|m| (m.node.clone(), m.clone()))
            .collect();
        // Parents sort before children (level first): a node marked
        // reviewed covers every changed node under it.
        let mut covered: HashSet<&NodeId> = HashSet::new();
        for (id, children) in &self.changed {
            if covered.contains(id) || marks.get(id).is_some_and(|m| m.reviewed) {
                covered.extend(children);
            }
        }
        // In reverse every child is settled before its parent.
        let mut reviewed = BTreeSet::new();
        for (id, children) in self.changed.iter().rev() {
            let marked = marks.get(id).is_some_and(|m| m.reviewed);
            let rolls_up =
                !self.own_mark.contains(id) && children.iter().all(|c| reviewed.contains(c));
            if marked || covered.contains(id) || rolls_up {
                reviewed.insert(id.clone());
            }
        }
        ReviewState {
            basis: Arc::clone(self),
            marks,
            reviewed,
        }
    }

    /// The key to store a mark on `id` under, or `None` if `id` did not change.
    #[must_use]
    pub fn key(&self, id: &NodeId) -> Option<u64> {
        self.keys.get(id).copied()
    }

    /// Every changed node, and every ancestor of one, ordered by id.
    pub fn changed(&self) -> impl Iterator<Item = &NodeId> {
        self.changed.keys()
    }

    /// Whether only `id`'s own mark can make it reviewed: it has no changed
    /// children, or changed itself (see [`ReviewState`]).
    #[must_use]
    pub fn needs_own_mark(&self, id: &NodeId) -> bool {
        self.own_mark.contains(id)
    }

    /// Whether `id` changed, itself or below it.
    #[must_use]
    pub fn is_changed(&self, id: &NodeId) -> bool {
        self.changed.contains_key(id)
    }
}

impl ReviewState {
    /// See [`ReviewBasis::key`].
    #[must_use]
    pub fn key(&self, id: &NodeId) -> Option<u64> {
        self.basis.key(id)
    }

    /// See [`ReviewBasis::changed`].
    pub fn changed(&self) -> impl Iterator<Item = &NodeId> {
        self.basis.changed()
    }

    /// See [`ReviewBasis::needs_own_mark`].
    #[must_use]
    pub fn needs_own_mark(&self, id: &NodeId) -> bool {
        self.basis.needs_own_mark(id)
    }

    /// See [`ReviewBasis::is_changed`].
    #[must_use]
    pub fn is_changed(&self, id: &NodeId) -> bool {
        self.basis.is_changed(id)
    }

    /// Whether `id` is reviewed, by its own mark or by roll-up.
    #[must_use]
    pub fn is_reviewed(&self, id: &NodeId) -> bool {
        self.reviewed.contains(id)
    }

    /// The mark on `id` that applies to this change, if any.
    #[must_use]
    pub fn mark(&self, id: &NodeId) -> Option<&Mark> {
        self.marks.get(id)
    }

    /// The changed nodes that only their own mark can make reviewed (no
    /// changed children, or a change of their own), and how many are
    /// reviewed: the review's progress. It is complete only when every
    /// changed node is reviewed.
    #[must_use]
    pub fn progress(&self) -> (usize, usize) {
        let own = &self.basis.own_mark;
        let done = own.iter().filter(|id| self.reviewed.contains(*id)).count();
        (done, own.len())
    }
}

/// 64-bit FNV-1a, with each field length-prefixed so fields cannot run together.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.bytes(s.as_bytes());
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

impl ReviewState {
    /// Whether a finding is reviewed: its node is, or for a cycle, every
    /// changed node on it is (an unchanged one has nothing to review).
    #[must_use]
    pub fn is_finding_reviewed(&self, item: &Item) -> bool {
        match item {
            Item::Cycle { nodes, .. } => {
                let mut changed = nodes.iter().filter(|n| self.is_changed(n)).peekable();
                changed.peek().is_some() && changed.all(|n| self.is_reviewed(n))
            }
            _ => item.target().is_some_and(|t| self.is_reviewed(t)),
        }
    }
}

/// The review of `diff` from `base_commit` to `head_commit` as Markdown, for
/// a pull request comment (#116): the findings in order, ticked where
/// reviewed; the reviewed nodes, each at the highest level reviewed; what is
/// still to review, at the highest level nothing has been reviewed under;
/// and every note. The app and `bp review export` both print this.
#[must_use]
pub fn export(
    state: &ReviewState,
    diff: &Diff,
    base: &Model,
    head: &Model,
    base_commit: &str,
    head_commit: &str,
) -> String {
    use std::fmt::Write as _;
    let basis = &state.basis;
    let short = |c: &str| c.chars().take(10).collect::<String>();
    let node = |id: &NodeId| {
        format!(
            "`{}` (L{})",
            findings::qualified(id, base, head).replace('`', "'"),
            id.level().number()
        )
    };
    let one_line = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    // Each changed node's changed parent.
    let mut parent: HashMap<&NodeId, &NodeId> = HashMap::new();
    for (id, children) in &basis.changed {
        for c in children {
            parent.insert(c, id);
        }
    }
    // Whether anything under each node is reviewed (children before parents).
    let mut touched: HashSet<&NodeId> = HashSet::new();
    for (id, children) in basis.changed.iter().rev() {
        if children
            .iter()
            .any(|c| state.is_reviewed(c) || touched.contains(c))
        {
            touched.insert(id);
        }
    }

    let (done, total) = state.progress();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "## Blueprint review: `{}` → `{}`\n",
        short(base_commit),
        short(head_commit)
    );
    let _ = writeln!(
        out,
        "Base `{base_commit}`, head `{head_commit}`. Reviewed {done} of {total} changes that need their own review.\n"
    );

    out.push_str("### Findings\n\n");
    let items = findings::items(diff, base, head);
    if items.is_empty() {
        out.push_str("None.\n");
    }
    for (n, item) in items.iter().enumerate() {
        let tick = if state.is_finding_reviewed(item) {
            "✓"
        } else {
            "☐"
        };
        let _ = writeln!(
            out,
            "{}. {tick} {}",
            n + 1,
            findings::label(item, base, head)
        );
    }

    let mut noted: HashSet<&NodeId> = HashSet::new();
    let note = |id: &NodeId| state.mark(id).and_then(|m| m.note.as_deref()).map(one_line);
    out.push_str("\n### Reviewed\n\n");
    let mut any = false;
    for id in basis.changed.keys() {
        let top = state.is_reviewed(id) && !parent.get(id).is_some_and(|p| state.is_reviewed(p));
        if top {
            any = true;
            let _ = match note(id) {
                Some(n) => {
                    noted.insert(id);
                    writeln!(out, "- {}: {n}", node(id))
                }
                None => writeln!(out, "- {}", node(id)),
            };
        }
    }
    if !any {
        out.push_str("Nothing yet.\n");
    }

    out.push_str("\n### Still to review\n\n");
    let mut open: Vec<String> = Vec::new();
    let mut stack: Vec<&NodeId> = basis
        .changed
        .keys()
        .filter(|id| !parent.contains_key(id))
        .rev()
        .collect();
    while let Some(id) = stack.pop() {
        if state.is_reviewed(id) {
            continue;
        }
        if !touched.contains(id) {
            open.push(format!("- {}", node(id)));
            continue;
        }
        // Partly reviewed: its own change, if any, then what is open below.
        if basis.own_mark.contains(id) {
            open.push(format!("- {}: its own change", node(id)));
        }
        if let Some(children) = basis.changed.get(id) {
            stack.extend(children.iter().rev());
        }
    }
    if open.is_empty() {
        out.push_str("Nothing: every change is reviewed.\n");
    }
    for line in open {
        let _ = writeln!(out, "{line}");
    }

    let notes: Vec<(&NodeId, String)> = basis
        .changed
        .keys()
        .filter(|id| !noted.contains(id))
        .filter_map(|id| note(id).map(|n| (id, n)))
        .collect();
    if !notes.is_empty() {
        out.push_str("\n### Notes\n\n");
        for (id, n) in notes {
            let status = if state.is_reviewed(id) {
                "reviewed"
            } else {
                "not reviewed"
            };
            let _ = writeln!(out, "- {} ({status}): {n}", node(id));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Confidence;
    use crate::diff::Weight;
    use crate::{Code, Ecosystem, Level, NodeKind, RepoPath, SystemKey, Visibility};

    fn sys() -> SystemKey {
        SystemKey {
            ecosystem: Ecosystem::Cargo,
            dir: RepoPath::root(),
        }
    }

    fn id(path: &[&str]) -> NodeId {
        let level = match path.len() {
            0 => return NodeId::system(sys()),
            1 => Level::Module,
            2 => Level::Component,
            _ => Level::Code,
        };
        NodeId::within(level, sys(), path.iter().map(|s| s.to_string()).collect())
    }

    /// A node at `path`, with `body` as its body hash below L2.
    fn node(path: &[&str], body: u64, first_line: u32) -> Node {
        let kind = match path.len() {
            0 => NodeKind::System,
            1 => NodeKind::Module,
            2 => NodeKind::Component,
            _ => NodeKind::Function,
        };
        Node {
            id: id(path),
            kind,
            name: path.last().copied().unwrap_or("a").into(),
            source: RepoPath::parse("src/lib.rs").unwrap(),
            parent: (!path.is_empty()).then(|| id(&path[..path.len() - 1])),
            confidence: Confidence::Exact,
            visibility: Some(Visibility::Public),
            code: (path.len() >= 2).then_some(Code {
                lines: (first_line, first_line + 3),
                body_line: Some(first_line + 1),
                signature_hash: 7,
                body_hash: Some(body),
            }),
        }
    }

    /// a › m › c › {f, g}, plus module `n` when `with_n`; `f` and `g`
    /// carry the given body hashes, and `shift` moves every line.
    fn model(f: u64, g: u64, with_n: Option<u64>, shift: u32) -> Model {
        let mut m = Model::default();
        let mut add = |n: Node| m.add_node(n).unwrap();
        add(node(&[], 0, 0));
        add(node(&["m"], 0, 0));
        add(node(&["m", "c"], 1, 1 + shift));
        add(node(&["m", "c", "f"], f, 2 + shift));
        add(node(&["m", "c", "g"], g, 6 + shift));
        if let Some(body) = with_n {
            add(node(&["n"], 0, 0));
            add(node(&["n", "d"], body, 1));
        }
        m
    }

    fn lines(body: u32) -> Lines {
        Lines { signature: 0, body }
    }

    /// `f` and `g` changed in body only; `c_own` extra lines in `c` itself.
    fn diff(c_own: u32) -> Diff {
        let w = |p: &[&str], body| Weight {
            id: id(p),
            lines: lines(body),
            digest: u64::from(body),
        };
        let total = 5 + c_own;
        Diff {
            nodes: vec![
                NodeChange::Contained {
                    id: id(&["m", "c", "f"]),
                },
                NodeChange::Contained {
                    id: id(&["m", "c", "g"]),
                },
            ],
            weights: vec![
                w(&[], total),
                w(&["m"], total),
                w(&["m", "c"], total),
                w(&["m", "c", "f"], 2),
                w(&["m", "c", "g"], 3),
            ],
            ..Diff::default()
        }
    }

    fn mark(state: &ReviewState, path: &[&str]) -> Mark {
        let node = id(path);
        Mark {
            key: state.key(&node).unwrap(),
            node,
            reviewed: true,
            note: None,
        }
    }

    #[test]
    fn a_parent_is_reviewed_when_all_its_changed_children_are() {
        let (base, head, diff) = (model(1, 2, None, 0), model(10, 20, None, 0), diff(0));
        let blank = ReviewState::new(&diff, &base, &head, &[]);
        assert!(blank.is_changed(&id(&["m"])));
        assert_eq!(blank.progress(), (0, 2));

        let f = mark(&blank, &["m", "c", "f"]);
        let one = ReviewState::new(&diff, &base, &head, [&f]);
        assert!(one.is_reviewed(&id(&["m", "c", "f"])));
        assert!(!one.is_reviewed(&id(&["m", "c"])), "g is not reviewed yet");
        assert_eq!(one.progress(), (1, 2));

        let g = mark(&blank, &["m", "c", "g"]);
        let both = ReviewState::new(&diff, &base, &head, [&f, &g]);
        for p in [&["m", "c"][..], &["m"], &[]] {
            assert!(both.is_reviewed(&id(p)), "{p:?} rolls up");
        }
        assert_eq!(both.progress(), (2, 2));
    }

    #[test]
    fn a_mark_on_a_parent_covers_everything_changed_under_it() {
        let (base, head, diff) = (model(1, 2, None, 0), model(10, 20, None, 0), diff(1));
        let blank = ReviewState::new(&diff, &base, &head, &[]);
        let m = mark(&blank, &["m"]);
        let state = ReviewState::new(&diff, &base, &head, [&m]);
        for p in [
            &["m"][..],
            &["m", "c"],
            &["m", "c", "f"],
            &["m", "c", "g"],
            &[],
        ] {
            assert!(state.is_reviewed(&id(p)), "{p:?}");
        }
        assert_eq!(state.progress(), (3, 3), "c's own change too");
    }

    #[test]
    fn a_change_of_the_parents_own_needs_its_own_mark() {
        let (base, head) = (model(1, 2, None, 0), model(10, 20, None, 0));
        let own_lines = diff(1);
        let mut surface = diff(0);
        surface.nodes.push(NodeChange::Surface {
            id: id(&["m", "c"]),
        });
        for diff in [own_lines, surface] {
            let blank = ReviewState::new(&diff, &base, &head, &[]);
            let marks = [
                mark(&blank, &["m", "c", "f"]),
                mark(&blank, &["m", "c", "g"]),
            ];
            let children = ReviewState::new(&diff, &base, &head, &marks);
            assert!(!children.is_reviewed(&id(&["m", "c"])), "{diff:?}");
            assert!(!children.is_reviewed(&id(&["m"])));
            assert_eq!(children.progress(), (2, 3), "c's own change counts");

            let all = [
                marks[0].clone(),
                marks[1].clone(),
                mark(&blank, &["m", "c"]),
            ];
            let state = ReviewState::new(&diff, &base, &head, &all);
            assert!(state.is_reviewed(&id(&["m", "c"])));
            assert!(state.is_reviewed(&id(&["m"])), "m has no lines of its own");
        }
    }

    #[test]
    fn a_rebase_keeps_marks_whose_change_is_unchanged_and_clears_the_rest() {
        let diff = diff(0);
        let (base, head) = (model(1, 2, None, 0), model(10, 20, None, 0));
        let blank = ReviewState::new(&diff, &base, &head, &[]);
        let marks = [
            mark(&blank, &["m", "c", "f"]),
            mark(&blank, &["m", "c", "g"]),
            mark(&blank, &[]),
        ];

        // Rebased onto a base where module n changed and lines in c moved:
        // f and g changed exactly as before, so their marks hold.
        let (base2, head2) = (model(1, 2, Some(5), 4), model(10, 20, Some(5), 4));
        let rebased = ReviewState::new(&diff, &base2, &head2, &marks);
        assert!(rebased.is_reviewed(&id(&["m", "c", "f"])));
        assert!(rebased.is_reviewed(&id(&["m", "c", "g"])));
        assert!(
            rebased.mark(&id(&[])).is_none(),
            "the system's content changed"
        );
        assert!(rebased.is_reviewed(&id(&[])), "but it still rolls up");

        // Rebased with f's change itself different: f's mark no longer applies.
        let head3 = model(11, 20, Some(5), 4);
        let changed = ReviewState::new(&diff, &base2, &head3, &marks);
        assert!(!changed.is_reviewed(&id(&["m", "c", "f"])));
        assert!(changed.is_reviewed(&id(&["m", "c", "g"])));
        assert!(!changed.is_reviewed(&id(&["m", "c"])));
    }

    #[test]
    fn unchanged_nodes_take_no_marks_and_notes_stand_alone() {
        let diff = diff(0);
        let (base, head) = (model(1, 2, Some(5), 0), model(10, 20, Some(5), 0));
        let blank = ReviewState::new(&diff, &base, &head, &[]);
        assert!(!blank.is_changed(&id(&["n"])));
        assert_eq!(blank.key(&id(&["n"])), None);

        let mut note = mark(&blank, &["m", "c", "f"]);
        note.reviewed = false;
        note.note = Some("check the bounds".into());
        let state = ReviewState::new(&diff, &base, &head, [&note]);
        assert!(!state.is_reviewed(&id(&["m", "c", "f"])));
        let kept = state
            .mark(&id(&["m", "c", "f"]))
            .and_then(|m| m.note.as_deref());
        assert_eq!(kept, Some("check the bounds"));
    }

    #[test]
    fn a_renamed_node_is_reviewed_under_its_head_id_and_rolls_up() {
        // f renamed to f2: the engine weighs f's removed lines under f and
        // f2's added lines under f2.
        let base = model(1, 2, None, 0);
        let mut head = Model::default();
        for n in [
            node(&[], 0, 0),
            node(&["m"], 0, 0),
            node(&["m", "c"], 1, 1),
            node(&["m", "c", "f2"], 1, 2),
            node(&["m", "c", "g"], 2, 6),
        ] {
            head.add_node(n).unwrap();
        }
        let w = |p: &[&str], body: u32| Weight {
            id: id(p),
            lines: lines(body),
            digest: u64::from(body) + p.len() as u64,
        };
        let diff = Diff {
            nodes: vec![NodeChange::Renamed {
                from: id(&["m", "c", "f"]),
                to: id(&["m", "c", "f2"]),
            }],
            weights: vec![
                w(&[], 2),
                w(&["m"], 2),
                w(&["m", "c"], 2),
                w(&["m", "c", "f"], 1),
                w(&["m", "c", "f2"], 1),
            ],
            ..Diff::default()
        };
        let blank = ReviewState::new(&diff, &base, &head, &[]);
        assert!(!blank.is_changed(&id(&["m", "c", "f"])), "f is f2");
        assert_eq!(blank.progress(), (0, 1));
        let f2 = mark(&blank, &["m", "c", "f2"]);
        let state = ReviewState::new(&diff, &base, &head, [&f2]);
        assert!(state.is_reviewed(&id(&["m", "c"])));
        assert!(state.is_reviewed(&id(&[])));
        assert_eq!(state.progress(), (1, 1));
    }

    #[test]
    fn a_type_moved_to_another_module_is_one_node_under_its_head_id() {
        // m::c with f and g moves to n::c; n exists on both sides.
        let mut base = Model::default();
        let mut head = Model::default();
        for n in [&[][..], &["m"], &["n"]] {
            base.add_node(node(n, 0, 0)).unwrap();
            head.add_node(node(n, 0, 0)).unwrap();
        }
        for (model, at) in [(&mut base, "m"), (&mut head, "n")] {
            model.add_node(node(&[at, "c"], 1, 1)).unwrap();
            model.add_node(node(&[at, "c", "f"], 2, 2)).unwrap();
            model.add_node(node(&[at, "c", "g"], 3, 6)).unwrap();
        }
        let moved = |p: &[&str]| NodeChange::Moved {
            from: id(&[&["m"][..], p].concat()),
            to: id(&[&["n"][..], p].concat()),
        };
        let w = |p: &[&str], body: u32| Weight {
            id: id(p),
            lines: lines(body),
            digest: u64::from(body) * 31 + p.len() as u64,
        };
        let diff = Diff {
            nodes: vec![moved(&["c"]), moved(&["c", "f"]), moved(&["c", "g"])],
            weights: vec![
                w(&[], 10),
                w(&["m"], 5),
                w(&["m", "c"], 5),
                w(&["m", "c", "f"], 2),
                w(&["m", "c", "g"], 2),
                w(&["n"], 5),
                w(&["n", "c"], 5),
                w(&["n", "c", "f"], 2),
                w(&["n", "c", "g"], 2),
            ],
            ..Diff::default()
        };
        let blank = ReviewState::new(&diff, &base, &head, &[]);
        for gone in [&["m", "c"][..], &["m", "c", "f"], &["m", "c", "g"]] {
            assert!(!blank.is_changed(&id(gone)), "{gone:?} is reviewed as n's");
        }
        assert_eq!(blank.progress(), (0, 3));
        let marks = [
            mark(&blank, &["n", "c"]),
            mark(&blank, &["n", "c", "f"]),
            mark(&blank, &["n", "c", "g"]),
        ];
        let state = ReviewState::new(&diff, &base, &head, &marks);
        for p in [&[][..], &["m"], &["n"]] {
            assert!(state.is_reviewed(&id(p)), "{p:?} rolls up");
        }
        assert_eq!(state.progress(), (3, 3));
    }

    #[test]
    fn a_mark_does_not_carry_to_another_change_of_a_node_without_code() {
        // A module's own lines (no code hashes): the same id and content
        // fingerprint, but different changed text, is a different change.
        let (base, head) = (model(1, 2, None, 0), model(1, 2, None, 0));
        let change = |digest: u64| Diff {
            nodes: vec![NodeChange::Changed { id: id(&["m"]) }],
            weights: vec![
                Weight {
                    id: id(&[]),
                    lines: lines(1),
                    digest,
                },
                Weight {
                    id: id(&["m"]),
                    lines: lines(1),
                    digest,
                },
            ],
            ..Diff::default()
        };
        let first = change(11);
        let m = mark(&ReviewState::new(&first, &base, &head, &[]), &["m"]);
        assert!(ReviewState::new(&first, &base, &head, [&m]).is_reviewed(&id(&["m"])));
        let other = ReviewState::new(&change(12), &base, &head, [&m]);
        assert!(!other.is_reviewed(&id(&["m"])));
    }

    #[test]
    fn the_export_lists_findings_reviewed_nodes_what_is_open_and_notes() {
        let (base, head, diff) = (model(1, 2, None, 0), model(10, 20, None, 0), diff(0));
        let blank = ReviewState::new(&diff, &base, &head, &[]);
        let mut f = mark(&blank, &["m", "c", "f"]);
        f.note = Some("bounds\nchecked".into());
        let mut g = mark(&blank, &["m", "c", "g"]);
        g.reviewed = false;
        g.note = Some("why 20?".into());
        let state = ReviewState::new(&diff, &base, &head, [&f, &g]);
        let text = export(
            &state,
            &diff,
            &base,
            &head,
            "0123456789abcdef",
            "fedcba9876543210",
        );
        let expected = "\
## Blueprint review: `0123456789` → `fedcba9876`

Base `0123456789abcdef`, head `fedcba9876543210`. Reviewed 1 of 2 changes that need their own review.

### Findings

1. ☐ Change concentrated in a › g: 3 lines (0 in signatures)
2. ✓ Change concentrated in a › f: 2 lines (0 in signatures)

### Reviewed

- `a › f` (L4): bounds checked

### Still to review

- `a › g` (L4)

### Notes

- `a › g` (L4) (not reviewed): why 20?
";
        assert_eq!(text, expected, "{text}");
    }

    #[test]
    fn marks_round_trip_as_json() {
        let m = Mark {
            node: id(&["m", "c"]),
            key: u64::MAX,
            reviewed: true,
            note: Some("ok".into()),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Mark>(&json).unwrap(), m);
    }
}
