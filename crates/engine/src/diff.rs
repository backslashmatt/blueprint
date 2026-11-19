//! The architectural diff of two models (REQUIREMENTS §3.3, DR 0008
//! decision 6): what changed in nodes and edges, and the findings a
//! reviewer should see first.
//!
//! - Nodes are compared by id. A removed and an added node left unmatched
//!   are paired when they have the same kind and the same body hash: a
//!   **move** if their parents differ, a **rename** if only the name does.
//!   A node with no body (a struct, an enum) pairs by its whole definition
//!   instead, kind, name and signature, which can only be a move.
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

pub use blueprint_model::diff::{Diff, Finding, Lines, NodeChange, Weight};

/// The diff from `base` to `head`.
#[must_use]
pub fn diff(base: &Model, head: &Model) -> Diff {
    // Both models iterate in id order, so one merge walk pairs everything:
    // no lookups, one comparison per step.
    let edges = merge(base.edges(), head.edges(), |e| &e.key);
    let added_edges: Vec<EdgeKey> = edges.only_head.iter().map(|e| e.key.clone()).collect();
    let removed_edges: Vec<EdgeKey> = edges.only_base.iter().map(|e| e.key.clone()).collect();
    // Nodes an added or removed edge touches: their body change is not
    // contained.
    let mut touched: BTreeSet<&NodeId> = BTreeSet::new();
    for k in added_edges.iter().chain(&removed_edges) {
        touched.insert(&k.from);
        touched.insert(&k.to);
    }

    let nodes_m = merge(base.nodes(), head.nodes(), |n| &n.id);
    let mut nodes: Vec<NodeChange> = nodes_m
        .both
        .iter()
        .filter_map(|(b, h)| changed(b, h, touched.contains(&b.id)))
        .collect();
    nodes.extend(pair(&nodes_m.only_base, &nodes_m.only_head));

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
        weights: Vec::new(),
    }
}

/// What a merge walk of two sorted sequences found.
struct Merged<'a, T> {
    only_base: Vec<&'a T>,
    only_head: Vec<&'a T>,
    both: Vec<(&'a T, &'a T)>,
}

/// Walks two sequences sorted by `key` together.
fn merge<'a, T: 'a, K: Ord + 'a>(
    base: impl Iterator<Item = &'a T>,
    head: impl Iterator<Item = &'a T>,
    key: impl Fn(&'a T) -> &'a K,
) -> Merged<'a, T> {
    let mut out = Merged {
        only_base: Vec::new(),
        only_head: Vec::new(),
        both: Vec::new(),
    };
    let (mut b, mut h) = (base.peekable(), head.peekable());
    loop {
        match (b.peek(), h.peek()) {
            (Some(&x), Some(&y)) => match key(x).cmp(key(y)) {
                std::cmp::Ordering::Less => {
                    out.only_base.push(x);
                    b.next();
                }
                std::cmp::Ordering::Greater => {
                    out.only_head.push(y);
                    h.next();
                }
                std::cmp::Ordering::Equal => {
                    out.both.push((x, y));
                    b.next();
                    h.next();
                }
            },
            (Some(&x), None) => {
                out.only_base.push(x);
                b.next();
            }
            (None, Some(&y)) => {
                out.only_head.push(y);
                h.next();
            }
            (None, None) => return out,
        }
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

/// Pairs removed and added nodes as moves and renames; the rest stay
/// removed and added. Pairing is greedy in id order, so it is deterministic.
///
/// - A node with a body pairs with one of the same kind and body hash.
/// - A node without one (a struct, an enum, a unit type) pairs with one of
///   the same kind, name and signature hash: its whole definition. That can
///   only be a move, since a different name is a different signature.
fn pair(removed: &[&Node], added: &[&Node]) -> Vec<NodeChange> {
    use blueprint_model::NodeKind;
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum Key<'a> {
        Body(NodeKind, u64),
        Whole(NodeKind, &'a str, u64),
    }
    fn key_of(n: &Node) -> Option<Key<'_>> {
        let code = n.code.as_ref()?;
        Some(match code.body_hash {
            Some(b) => Key::Body(n.kind, b),
            None => Key::Whole(n.kind, n.name.as_str(), code.signature_hash),
        })
    }
    let mut by_key: BTreeMap<Key<'_>, Vec<&Node>> = BTreeMap::new();
    for a in added {
        if let Some(k) = key_of(a) {
            by_key.entry(k).or_default().push(a);
        }
    }
    let mut out = Vec::new();
    let mut paired: BTreeSet<&NodeId> = BTreeSet::new();
    for r in removed {
        let candidate = key_of(r)
            .and_then(|k| by_key.get(&k))
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

/// A file that differs between the two commits: its bytes in base and in
/// head, `None` where it does not exist.
pub struct ChangedFile<'a> {
    /// The file.
    pub path: &'a blueprint_model::RepoPath,
    /// Its bytes in base.
    pub base: Option<&'a [u8]>,
    /// Its bytes in head.
    pub head: Option<&'a [u8]>,
}

/// Fills in `d.weights` from a line diff of each changed file. Every
/// removed line counts against the innermost base node whose span holds
/// it, and every added line against the innermost head node; lines no item
/// holds count for the file's own node (its module, or the component of a
/// file no analyser reads). Weights then roll up each model's parents, and
/// the two are summed by id.
pub fn weigh(d: &mut Diff, base: &Model, head: &Model, files: &[ChangedFile<'_>]) {
    use imara_diff::{Algorithm, InternedInput};
    let mut own: BTreeMap<(bool, NodeId), (Lines, u64)> = BTreeMap::new();
    let paths: BTreeSet<&blueprint_model::RepoPath> = files.iter().map(|f| f.path).collect();
    let by_base = by_source(base, &paths);
    let by_head = by_source(head, &paths);
    for f in files {
        let b = f.base.unwrap_or_default();
        let h = f.head.unwrap_or_default();
        let input = InternedInput::new(
            imara_diff::sources::byte_lines(b),
            imara_diff::sources::byte_lines(h),
        );
        let mut diff = imara_diff::Diff::compute(Algorithm::Histogram, &input);
        diff.postprocess_lines(&input);
        let removed: Vec<u32> = (0..input.before.len() as u32)
            .filter(|&i| diff.is_removed(i))
            .map(|i| i + 1)
            .collect();
        let added: Vec<u32> = (0..input.after.len() as u32)
            .filter(|&i| diff.is_added(i))
            .map(|i| i + 1)
            .collect();
        let (b_text, h_text) = (split_lines(b), split_lines(h));
        for (lines, text, nodes, in_base) in [
            (&removed, &b_text, by_base.get(f.path), true),
            (&added, &h_text, by_head.get(f.path), false),
        ] {
            if lines.is_empty() {
                continue;
            }
            let nodes = nodes.map_or(&[][..], Vec::as_slice);
            for (id, w, digest) in attribute(lines, text, nodes) {
                let entry = own.entry((in_base, id)).or_insert((
                    Lines {
                        signature: 0,
                        body: 0,
                    },
                    0,
                ));
                entry.0.add(w);
                // Files in any order: combined so the order does not matter.
                entry.1 ^= mix(fnv(f.path.to_string().as_bytes()), digest);
            }
        }
    }
    // Roll up each model's own weights through its own parents.
    let mut total: BTreeMap<NodeId, (Lines, u64)> = BTreeMap::new();
    for (model, in_base) in [(base, true), (head, false)] {
        for ((_, id), (w, digest)) in own
            .iter()
            .filter(|((b, id), _)| *b == in_base && model.node(id).is_some())
        {
            // Each node's own change, tagged with its side and id, so
            // equal text in two places or on two sides does not cancel out.
            let part = mix(
                mix(u64::from(in_base), fnv(id.to_string().as_bytes())),
                *digest,
            );
            let mut cur = Some(id.clone());
            while let Some(c) = cur {
                let t = total.entry(c.clone()).or_insert((
                    Lines {
                        signature: 0,
                        body: 0,
                    },
                    0,
                ));
                t.0.add(*w);
                t.1 ^= part;
                cur = model.node(&c).and_then(|n| n.parent.clone());
            }
        }
    }
    d.weights = total
        .into_iter()
        .filter(|(_, (w, _))| !w.is_zero())
        .map(|(id, (lines, digest))| Weight { id, lines, digest })
        .collect();
}

/// The nodes each file holds that can take a line: items with a span, and
/// the file's own module or component.
fn by_source<'m>(
    m: &'m Model,
    paths: &BTreeSet<&blueprint_model::RepoPath>,
) -> BTreeMap<&'m blueprint_model::RepoPath, Vec<&'m Node>> {
    let mut out: BTreeMap<&blueprint_model::RepoPath, Vec<&Node>> = BTreeMap::new();
    for n in m.nodes().filter(|n| paths.contains(&n.source)) {
        let file_node = matches!(n.kind, blueprint_model::NodeKind::Module)
            || (n.code.is_none()
                && n.confidence == blueprint_model::Confidence::Unknown
                && n.level() == Level::Component);
        if n.code.is_some() || file_node {
            out.entry(&n.source).or_default().push(n);
        }
    }
    out
}

/// Each changed line's innermost item, or else the file's own node.
fn attribute(lines: &[u32], text: &[&[u8]], nodes: &[&Node]) -> Vec<(NodeId, Lines, u64)> {
    // Items by span length, so the innermost claims a line first.
    let mut items: Vec<(&Node, (u32, u32), Option<u32>)> = nodes
        .iter()
        .filter_map(|n| n.code.as_ref().map(|c| (*n, c.lines, c.body_line)))
        .collect();
    items.sort_by_key(|(n, (a, b), _)| (b - a, n.id.clone()));
    // Per node: its weight, its digest, and its last changed line.
    let mut out: BTreeMap<NodeId, (Lines, u64, Option<u32>)> = BTreeMap::new();
    let file = nodes
        .iter()
        .filter(|n| n.code.is_none())
        .min_by_key(|n| (n.id.path().len(), n.id.clone()));
    for &line in lines {
        let hit = items.iter().find(|(_, (a, b), _)| *a <= line && line <= *b);
        let (id, signature) = match hit {
            // A signature usually shares its line with the body's opening
            // brace: the line the item starts on counts as signature.
            Some((n, (start, _), body)) => (
                n.id.clone(),
                line == *start || body.is_none_or(|b| line < b),
            ),
            None => match file {
                Some(n) => (n.id.clone(), false),
                None => continue,
            },
        };
        let (w, digest, prev) = out.entry(id).or_insert((
            Lines {
                signature: 0,
                body: 0,
            },
            fnv(b""),
            None,
        ));
        if signature {
            w.signature += 1;
        } else {
            w.body += 1;
        }
        // The node's changed lines in order, without their numbers, each
        // run of them with the unchanged line just above it, so the same
        // text changed in another place is another change.
        let at = |n: u32| {
            text.get((n as usize).wrapping_sub(1))
                .copied()
                .unwrap_or_default()
        };
        if prev.is_none_or(|p| p + 1 != line) {
            *digest = mix(*digest, mix(1, fnv(at(line.wrapping_sub(1)))));
        }
        *prev = Some(line);
        *digest = mix(*digest, fnv(at(line)));
    }
    out.into_iter().map(|(id, (w, d, _))| (id, w, d)).collect()
}

/// A file's lines, each with its line ending, as `byte_lines` splits them.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|&b| b == b'\n').collect()
}

/// 64-bit FNV-1a.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Combines two hashes, in order.
fn mix(a: u64, b: u64) -> u64 {
    fnv(&[a.to_le_bytes(), b.to_le_bytes()].concat())
}

#[cfg(test)]
mod tests {
    use super::tarjan;

    /// The digest of `f`'s weight, diffing `lib.rs` from `base` to `head`.
    fn digest_of_f(base: &str, head: &str) -> u64 {
        use blueprint_model::RepoPath;
        let lib = RepoPath::parse("src/lib.rs").unwrap_or_else(|_| RepoPath::root());
        let manifest = RepoPath::parse("Cargo.toml").unwrap_or_else(|_| RepoPath::root());
        let paths = [manifest.clone(), lib.clone()];
        let model = |text: &str| {
            let text = text.to_string();
            let lib = lib.clone();
            let read = move |p: &RepoPath| {
                Some(if *p == lib {
                    text.clone().into_bytes()
                } else {
                    b"[package]\nname = \"a\"\n".to_vec()
                })
            };
            crate::analyse_deep(&paths, &read).0
        };
        let (b, h) = (model(base), model(head));
        let mut d = super::diff(&b, &h);
        let file = super::ChangedFile {
            path: &lib,
            base: Some(base.as_bytes()),
            head: Some(head.as_bytes()),
        };
        super::weigh(&mut d, &b, &h, &[file]);
        let f = d.weights.iter().find(|w| w.id.to_string().ends_with("/f"));
        f.map_or(0, |w| w.digest)
    }

    #[test]
    fn a_weights_digest_is_the_changed_text_not_its_place() {
        let one = digest_of_f(
            "pub fn f() -> u8 {\n    1\n}\n",
            "pub fn f() -> u8 {\n    2\n}\n",
        );
        assert_ne!(one, 0);
        let moved = digest_of_f(
            "// a\n// b\npub fn f() -> u8 {\n    1\n}\n",
            "// a\n// b\npub fn f() -> u8 {\n    2\n}\n",
        );
        assert_eq!(one, moved, "the same change two lines lower");
        let other = digest_of_f(
            "pub fn f() -> u8 {\n    1\n}\n",
            "pub fn f() -> u8 {\n    3\n}\n",
        );
        assert_ne!(one, other, "a different change");
        let elsewhere = digest_of_f(
            "pub fn f() -> u8 {\n    let x = 0;\n    1\n}\n",
            "pub fn f() -> u8 {\n    let x = 0;\n    2\n}\n",
        );
        assert_ne!(one, elsewhere, "the same edit after other code");
    }

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
