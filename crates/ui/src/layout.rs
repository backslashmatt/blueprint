//! Map layout (REQUIREMENTS §5.4, DR 0010): one view is the children of
//! one node, laid out in layers with orthogonal edges. Deterministic: the
//! same nodes and edges always give the same layout, whatever order they
//! are given in. Headless; the map view draws what this returns.
//!
//! 1. Nodes with no edge in the view are packed in a grid to the right of
//!    the graph, sorted by id, so that the edge-free views of tens of
//!    thousands of nodes (test directories) cost almost nothing.
//! 2. Cycles are broken by a depth-first search in id order: an edge back
//!    to a node on the search stack is laid out reversed.
//! 3. Each node's layer is its longest path from a node nothing depends
//!    on (layer 0, at the top), so a node is drawn above everything it
//!    depends on. Edges spanning several layers get a
//!    virtual node in each layer they cross.
//! 4. Within a layer, nodes start in id order and are reordered by
//!    barycentre over [`SWEEPS`] passes, ties kept in their earlier order.
//! 5. Each layer is centred; edges leave the bottom of their source, run
//!    horizontally in the channel below that layer, and enter the top of
//!    the next node on their way.

use std::collections::{BTreeMap, HashMap};

/// Barycentre passes (down and up count as one each).
pub const SWEEPS: usize = 8;

/// A node's size and the gaps between nodes, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Node width.
    pub width: f32,
    /// Node height.
    pub height: f32,
    /// Horizontal gap between nodes in a layer.
    pub gap: f32,
    /// Vertical gap between layers, where edges run.
    pub channel: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            width: 160.0,
            height: 36.0,
            gap: 24.0,
            channel: 48.0,
        }
    }
}

/// A point in the view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Left to right.
    pub x: f32,
    /// Top to bottom.
    pub y: f32,
}

/// The layout of one view.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    /// Each node's top-left corner, in the order the nodes were given.
    pub nodes: Vec<Point>,
    /// Each edge as an orthogonal polyline from its source to its target,
    /// in the order the edges were given.
    pub edges: Vec<Vec<Point>>,
    /// The size of everything laid out.
    pub width: f32,
    /// See `width`.
    pub height: f32,
    /// Each node's cell in the grid of nodes without edges, if it is in it.
    cells: Vec<Option<usize>>,
    /// The grid's left edge and its number of columns, if there is a grid.
    grid: Option<(f32, usize)>,
    /// The metrics it was laid out with.
    metrics: Option<Metrics>,
    /// Each bend of a long edge: the edge's ends (indices into the ids,
    /// drawn top to bottom), its layer and its x.
    bends: Vec<(usize, usize, usize, f32)>,
}

/// Where each node of an earlier layout was, so that a new layout of
/// nearly the same view keeps them there (see [`relayout`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Seed {
    metrics: Option<Metrics>,
    at: HashMap<String, (Point, Option<usize>)>,
    grid: Option<(f32, usize)>,
    bends: HashMap<(String, String, usize), f32>,
}

impl Layout {
    /// What [`relayout`] needs to keep this layout's nodes in place.
    /// `ids` must be the ids it was laid out with, in the same order.
    #[must_use]
    pub fn seed(&self, ids: &[&str]) -> Seed {
        Seed {
            metrics: self.metrics,
            at: ids
                .iter()
                .zip(&self.nodes)
                .zip(&self.cells)
                .map(|((id, p), c)| ((*id).to_owned(), (*p, *c)))
                .collect(),
            grid: self.grid,
            bends: self
                .bends
                .iter()
                .filter(|(a, b, _, _)| *a < ids.len() && *b < ids.len())
                .map(|(a, b, l, x)| ((ids[*a].to_owned(), ids[*b].to_owned(), *l), *x))
                .collect(),
        }
    }
}

/// Lays out `ids` (unique) with `edges` between them as `(from, to)` index
/// pairs, `from` depending on `to`. Self-loops and duplicate edges are
/// ignored.
#[must_use]
pub fn layout(ids: &[&str], edges: &[(usize, usize)], m: Metrics) -> Layout {
    lay_out(ids, edges, m, None)
}

/// As [`layout`], but every node that `seed` placed keeps its horizontal
/// place in its layer, or its grid cell, unless a new node has to push it
/// aside; new nodes go next to their neighbours, or after the others. For
/// a view that changed a little since `seed` was taken: a small change
/// moves few nodes (§5.4). Nodes still move down when a new edge deepens
/// the layer they are in.
#[must_use]
pub fn relayout(ids: &[&str], edges: &[(usize, usize)], m: Metrics, seed: &Seed) -> Layout {
    lay_out(ids, edges, m, Some(seed))
}

/// Most virtual nodes a layout makes, per node and edge. Beyond it, long
/// edges are drawn straight down without bends, which can cross nodes, so
/// that a deep graph (a chain with a common sink) stays fast.
const VIRTUAL_PER_ITEM: usize = 8;

/// `m` with every value finite and positive (gaps may be zero), the default
/// standing in for any that is not.
fn sane(m: Metrics) -> Metrics {
    let d = Metrics::default();
    let pick = |v: f32, d: f32, min: f32| {
        if v.is_finite() && v >= min && v < 1e6 {
            v
        } else {
            d
        }
    };
    Metrics {
        width: pick(m.width, d.width, 1.0),
        height: pick(m.height, d.height, 1.0),
        gap: pick(m.gap, d.gap, 0.0),
        channel: pick(m.channel, d.channel, 0.0),
    }
}

fn lay_out(ids: &[&str], edges: &[(usize, usize)], m: Metrics, seed: Option<&Seed>) -> Layout {
    let m = sane(m);
    // A seed taken with other metrics places nodes on another grid.
    let seed = seed.filter(|s| s.metrics == Some(m));
    let n = ids.len();
    // Work in id order, so the input order never matters.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| ids[*a].cmp(ids[*b]));
    let mut rank = vec![0; n];
    for (r, i) in order.iter().enumerate() {
        rank[*i] = r;
    }
    // Edges in rank space, deduplicated and sorted.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a < n && b < n && a != b {
            out[rank[a]].push(rank[b]);
        }
    }
    for o in &mut out {
        o.sort_unstable();
        o.dedup();
    }
    let connected: Vec<bool> = {
        let mut c = vec![false; n];
        for (a, o) in out.iter().enumerate() {
            if !o.is_empty() {
                c[a] = true;
            }
            for &b in o {
                c[b] = true;
            }
        }
        c
    };

    let reversed = feedback_edges(&out);
    // The acyclic graph: `down[a]` are the nodes drawn below `a`.
    let mut down: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (a, o) in out.iter().enumerate() {
        for &b in o {
            if reversed.contains(&(a, b)) {
                down[b].push(a);
            } else {
                down[a].push(b);
            }
        }
    }
    for d in &mut down {
        d.sort_unstable();
        d.dedup();
    }
    // Seeded, a node stays in its old layer unless an edge now needs it
    // deeper.
    let floor: Vec<usize> = (0..n)
        .map(|r| match seed.and_then(|s| s.at.get(ids[order[r]])) {
            Some((p, None)) => {
                let l = (p.y / (m.height + m.channel)).round();
                if l.is_finite() && l > 0.0 {
                    (l as usize).min(n)
                } else {
                    0
                }
            }
            _ => 0,
        })
        .collect();
    let layer = layers(&down, &connected, &floor);

    // Virtual nodes: one per layer an edge crosses. `chain[(a, b)]` lists
    // the nodes (real and virtual) an edge passes through, top to bottom.
    let depth = layer.iter().copied().max().map_or(0, |d| d + 1);
    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for r in 0..n {
        if connected[r] {
            rows[layer[r]].push(r);
        }
    }
    let mut next_id = n;
    let mut up_links: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut down_links: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut chain: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
    // Each virtual node's edge (in rank space) and layer.
    let mut bend_of: Vec<(usize, usize, usize)> = Vec::new();
    let spans: usize = (0..n)
        .flat_map(|a| down[a].iter().map(move |&b| (a, b)))
        .map(|(a, b)| layer[b].saturating_sub(layer[a] + 1))
        .sum();
    let edge_count: usize = down.iter().map(Vec::len).sum();
    let bends = spans <= VIRTUAL_PER_ITEM * (n + edge_count) + 1024;
    for a in 0..n {
        for &b in &down[a] {
            let mut path = vec![a];
            let mut prev = a;
            let (from, to) = if bends {
                (layer[a] + 1, layer[b])
            } else {
                (0, 0)
            };
            for (l, row) in rows.iter_mut().enumerate().take(to).skip(from) {
                let v = next_id;
                next_id += 1;
                bend_of.push((a, b, l));
                row.push(v);
                down_links.entry(prev).or_default().push(v);
                up_links.entry(v).or_default().push(prev);
                path.push(v);
                prev = v;
            }
            down_links.entry(prev).or_default().push(b);
            up_links.entry(b).or_default().push(prev);
            path.push(b);
            chain.insert((a, b), path);
        }
    }

    let row_width = |k: usize| k as f32 * (m.width + m.gap) - m.gap;
    let step = m.width + m.gap;
    let mut pos = vec![Point { x: 0.0, y: 0.0 }; next_id];
    let seeded = |r: usize| seed.and_then(|s| s.at.get(ids[order[r]]));
    let widest = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut full = row_width(widest).max(0.0);
    if let Some(seed) = seed {
        // Keys: a seeded node's old x; a new node the mean x of its seeded
        // neighbours, or after everything; a virtual node in between the
        // ends of its edge.
        let mut key = vec![f32::NAN; next_id];
        let old_right = seed.at.values().map(|(p, _)| p.x).fold(0.0f32, f32::max);
        for r in 0..n {
            if !connected[r] {
                continue;
            }
            key[r] = match seeded(r) {
                Some((p, None)) => p.x,
                _ => {
                    let near: Vec<f32> = out[r]
                        .iter()
                        .chain(down_links.get(&r).into_iter().flatten())
                        .chain(up_links.get(&r).into_iter().flatten())
                        .filter_map(|&w| (w < n).then(|| seeded(w)).flatten())
                        .filter(|(_, cell)| cell.is_none())
                        .map(|(p, _)| p.x)
                        .collect();
                    if near.is_empty() {
                        old_right + step
                    } else {
                        near.iter().sum::<f32>() / near.len() as f32
                    }
                }
            };
        }
        let old_bend = |v: usize| {
            let (a, b, l) = bend_of[v - n];
            seed.bends
                .get(&(ids[order[a]].to_owned(), ids[order[b]].to_owned(), l))
                .copied()
        };
        for path in chain.values() {
            let (Some(&a), Some(&b)) = (path.first(), path.last()) else {
                continue;
            };
            let k = path.len() - 1;
            for (i, &v) in path.iter().enumerate().take(k).skip(1) {
                key[v] = old_bend(v).unwrap_or(key[a] + (key[b] - key[a]) * i as f32 / k as f32);
            }
        }
        full = 0.0;
        for (l, row) in rows.iter_mut().enumerate() {
            let y = l as f32 * (m.height + m.channel);
            // Nodes seeded in this same layer first, where they were: they
            // cannot overlap. Then the others, each in the free place
            // nearest its key, so that nothing already placed moves.
            let stays = |v: usize| {
                if v >= n {
                    return old_bend(v).is_some();
                }
                matches!(seeded(v), Some((p, None)) if (p.y - y).abs() < 0.5)
            };
            let (mut first, mut rest): (Vec<usize>, Vec<usize>) =
                row.iter().partition(|v| stays(**v));
            first.sort_by(|a, b| key[*a].total_cmp(&key[*b]));
            rest.sort_by(|a, b| key[*a].total_cmp(&key[*b]));
            let mut taken: Vec<f32> = Vec::with_capacity(row.len());
            for &v in &first {
                pos[v] = Point { x: key[v], y };
                taken.push(key[v]);
            }
            for &v in &rest {
                let x = nearest_free(&taken, key[v].max(0.0), step);
                let at = taken.partition_point(|t| *t < x);
                taken.insert(at, x);
                pos[v] = Point { x, y };
            }
            if let Some(last) = taken.last() {
                full = full.max(last + m.width);
            }
            row.sort_by(|a, b| pos[*a].x.total_cmp(&pos[*b].x));
        }
    } else {
        order_rows(&mut rows, &up_links, &down_links, next_id);
        // Layers centred on the widest.
        for (l, row) in rows.iter().enumerate() {
            let left = (full - row_width(row.len())) / 2.0;
            let y = l as f32 * (m.height + m.channel);
            for (i, &v) in row.iter().enumerate() {
                pos[v] = Point {
                    x: left + i as f32 * step,
                    y,
                };
            }
        }
    }
    let graph_height = if depth == 0 {
        0.0
    } else {
        depth as f32 * (m.height + m.channel) - m.channel
    };

    // Unconnected nodes: a grid to the right of the graph, in id order, so
    // that the graph growing taller never moves it. Seeded, each keeps its
    // cell, new ones take the first free cells, and the grid moves right
    // only if the graph has grown into it.
    let loose: Vec<usize> = (0..n).filter(|r| !connected[*r]).collect();
    let mut height = graph_height;
    let mut width = full;
    let mut cells = vec![None; n];
    let mut grid = None;
    if !loose.is_empty() {
        let fresh_cols = (loose.len() as f32).sqrt().ceil().max(1.0) as usize;
        let beside = if full > 0.0 { full + m.channel } else { 0.0 };
        let cols = seed
            .and_then(|s| s.grid)
            .map_or(fresh_cols, |(_, c)| c.max(1));
        let mut taken = std::collections::BTreeSet::new();
        let mut placed = Vec::new();
        let mut new = Vec::new();
        for &r in &loose {
            match seeded(r) {
                Some((_, Some(c))) if taken.insert(*c) => placed.push((r, *c)),
                _ => new.push(r),
            }
        }
        let mut free = (0..).filter(|c| !taken.contains(c));
        for r in new {
            if let Some(c) = free.next() {
                placed.push((r, c));
            }
        }
        let last = placed.iter().map(|(_, c)| *c).max().unwrap_or(0);
        let bottom = (last / cols + 1) as f32 * (m.height + m.gap);
        let left = match seed.and_then(|s| s.grid) {
            // It stays unless something in the graph now reaches into it.
            Some((left, _)) => {
                let reached = pos[..next_id]
                    .iter()
                    .enumerate()
                    .filter(|(v, _)| *v >= n || connected[*v])
                    .any(|(_, p)| p.x + m.width > left && p.y < bottom);
                if reached { left.max(beside) } else { left }
            }
            None => beside,
        };
        for (r, c) in placed {
            cells[r] = Some(c);
            pos[r] = Point {
                x: left + (c % cols) as f32 * step,
                y: (c / cols) as f32 * (m.height + m.gap),
            };
        }
        let rows_used = last / cols + 1;
        height = height.max(rows_used as f32 * (m.height + m.gap) - m.gap);
        width = width.max(left + row_width(cols.min(last + 1)));
        grid = Some((left, cols));
    }

    // Edges: down from the bottom centre, across in the channel, into the
    // top centre of the next node on the chain.
    let route = |path: &[usize]| -> Vec<Point> {
        let mut pts = Vec::new();
        for w in path.windows(2) {
            let (p, q) = (pos[w[0]], pos[w[1]]);
            let (from_x, to_x) = (p.x + m.width / 2.0, q.x + m.width / 2.0);
            // A virtual node is a point: its edge passes straight through.
            let from_y = if w[0] < n { p.y + m.height } else { p.y };
            let mid = p.y + m.height + m.channel / 2.0;
            if pts.is_empty() {
                pts.push(Point {
                    x: from_x,
                    y: from_y,
                });
            }
            pts.push(Point { x: from_x, y: mid });
            pts.push(Point { x: to_x, y: mid });
            pts.push(Point { x: to_x, y: q.y });
        }
        pts
    };
    let mut edge_routes = Vec::with_capacity(edges.len());
    for &(a, b) in edges {
        if a >= n || b >= n || a == b {
            edge_routes.push(Vec::new());
            continue;
        }
        let (ra, rb) = (rank[a], rank[b]);
        let points = if let Some(p) = chain.get(&(ra, rb)) {
            route(p)
        } else if let Some(p) = chain.get(&(rb, ra)) {
            // Laid out reversed to break a cycle: drawn from `to` up.
            let mut pts = route(p);
            pts.reverse();
            pts
        } else {
            Vec::new()
        };
        edge_routes.push(points);
    }

    Layout {
        nodes: (0..n).map(|i| pos[rank[i]]).collect(),
        edges: edge_routes,
        width,
        height: height.max(0.0),
        cells: (0..n).map(|i| cells[rank[i]]).collect(),
        grid,
        metrics: Some(m),
        bends: bend_of
            .iter()
            .enumerate()
            .map(|(i, (a, b, l))| (order[*a], order[*b], *l, pos[n + i].x))
            .collect(),
    }
}

/// The edges to reverse so the graph has no cycle: those that reach a node
/// still on the stack of a depth-first search started from each node in
/// order. Iterative, so deep graphs cannot overflow the stack.
fn feedback_edges(out: &[Vec<usize>]) -> std::collections::BTreeSet<(usize, usize)> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Open,
        Done,
    }
    let mut state = vec![State::New; out.len()];
    let mut reversed = std::collections::BTreeSet::new();
    for start in 0..out.len() {
        if state[start] != State::New {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        state[start] = State::Open;
        while let Some(&mut (v, ref mut next)) = stack.last_mut() {
            if let Some(&w) = out[v].get(*next) {
                *next += 1;
                match state[w] {
                    State::New => {
                        state[w] = State::Open;
                        stack.push((w, 0));
                    }
                    State::Open => {
                        reversed.insert((v, w));
                    }
                    State::Done => {}
                }
            } else {
                state[v] = State::Done;
                stack.pop();
            }
        }
    }
    reversed
}

/// Each connected node's layer: its `floor` (0 unless seeded), or one more
/// than the deepest node above it if that is deeper. Computed in
/// topological order.
fn layers(down: &[Vec<usize>], connected: &[bool], floor: &[usize]) -> Vec<usize> {
    let n = down.len();
    let mut indegree = vec![0usize; n];
    for d in down {
        for &b in d {
            indegree[b] += 1;
        }
    }
    let mut layer = floor.to_vec();
    let mut ready: std::collections::VecDeque<usize> = (0..n)
        .filter(|v| connected[*v] && indegree[*v] == 0)
        .collect();
    while let Some(v) = ready.pop_front() {
        for &b in &down[v] {
            layer[b] = layer[b].max(layer[v] + 1);
            indegree[b] -= 1;
            if indegree[b] == 0 {
                ready.push_back(b);
            }
        }
    }
    layer
}

/// The x nearest `want` where a node fits between the sorted lefts in
/// `taken`, `step` apart; to the right on a tie.
fn nearest_free(taken: &[f32], want: f32, step: f32) -> f32 {
    let fits = |x: f32| {
        let i = taken.partition_point(|t| *t < x);
        let left_ok = i == 0 || taken[i - 1] + step <= x + 1e-3;
        let right_ok = i == taken.len() || x + step <= taken[i] + 1e-3;
        left_ok && right_ok
    };
    // Appending to the right is the common case: keep it constant time.
    if let Some(&last) = taken.last()
        && want >= last
    {
        return want.max(last + step);
    }
    if fits(want) {
        return want;
    }
    // Walk right past each blocking node, then left.
    let mut right = want;
    let mut i = taken.partition_point(|t| *t + step <= right + 1e-3);
    while i < taken.len() && taken[i] < right + step - 1e-3 {
        right = right.max(taken[i] + step);
        i += 1;
    }
    let mut left = want;
    let mut j = taken.partition_point(|t| *t < left + step - 1e-3);
    while j > 0 && taken[j - 1] + step > left + 1e-3 {
        left = left.min(taken[j - 1] - step);
        j -= 1;
    }
    if left >= 0.0 && want - left < right - want {
        left
    } else {
        right
    }
}

/// Reorders each row by the mean position of its neighbours in the row
/// above (downward passes) or below (upward passes). A stable sort keeps
/// ties, and nodes with no neighbour there, in their earlier order.
fn order_rows(
    rows: &mut [Vec<usize>],
    up: &BTreeMap<usize, Vec<usize>>,
    down: &BTreeMap<usize, Vec<usize>>,
    total: usize,
) {
    let mut at = vec![0f32; total];
    let place = |rows: &[Vec<usize>], at: &mut Vec<f32>| {
        for row in rows {
            for (i, &v) in row.iter().enumerate() {
                at[v] = i as f32;
            }
        }
    };
    place(rows, &mut at);
    for sweep in 0..SWEEPS {
        let downward = sweep % 2 == 0;
        let indices: Vec<usize> = if downward {
            (1..rows.len()).collect()
        } else {
            (0..rows.len().saturating_sub(1)).rev().collect()
        };
        for l in indices {
            let links = if downward { up } else { down };
            let mut keyed: Vec<(f32, usize)> = rows[l]
                .iter()
                .map(|&v| {
                    let key = match links.get(&v) {
                        Some(ns) if !ns.is_empty() => {
                            ns.iter().map(|w| at[*w]).sum::<f32>() / ns.len() as f32
                        }
                        _ => at[v],
                    };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
            rows[l] = keyed.into_iter().map(|(_, v)| v).collect();
            for (i, &v) in rows[l].iter().enumerate() {
                at[v] = i as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay(ids: &[&str], edges: &[(usize, usize)]) -> Layout {
        layout(ids, edges, Metrics::default())
    }

    #[test]
    fn a_dependency_is_drawn_below_its_dependent() {
        let l = lay(&["app", "lib", "core"], &[(0, 1), (1, 2)]);
        assert!(l.nodes[0].y < l.nodes[1].y && l.nodes[1].y < l.nodes[2].y);
        let e = &l.edges[0];
        assert!(e.len() >= 2);
        assert!(
            e.windows(2).all(|w| w[0].x == w[1].x || w[0].y == w[1].y),
            "orthogonal: {e:?}"
        );
    }

    #[test]
    fn the_order_nodes_are_given_in_does_not_matter() {
        let a = lay(&["a", "b", "c", "d"], &[(0, 1), (0, 2), (2, 3), (1, 3)]);
        // The same graph, nodes given in another order.
        let b = lay(&["d", "c", "b", "a"], &[(3, 2), (3, 1), (1, 0), (2, 0)]);
        for (i, j) in [(0, 3), (1, 2), (2, 1), (3, 0)] {
            assert_eq!(a.nodes[i], b.nodes[j]);
        }
    }

    #[test]
    fn cycles_and_bad_edges_are_survived() {
        let l = lay(&["a", "b", "c"], &[(0, 1), (1, 2), (2, 0), (1, 1), (0, 9)]);
        assert_eq!(l.nodes.len(), 3);
        assert_eq!(l.edges.len(), 5);
        assert!(!l.edges[2].is_empty(), "the cycle's closing edge is drawn");
        assert!(l.edges[3].is_empty() && l.edges[4].is_empty());
    }

    #[test]
    fn nodes_without_edges_go_in_a_grid_beside() {
        let ids: Vec<String> = (0..100).map(|i| format!("n{i:03}")).collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let l = lay(&ids, &[(0, 1)]);
        let graph_right = l.nodes[0].x.max(l.nodes[1].x) + Metrics::default().width;
        assert!(l.nodes[2..].iter().all(|p| p.x > graph_right));
        let distinct: std::collections::BTreeSet<(u32, u32)> = l
            .nodes
            .iter()
            .map(|p| (p.x.to_bits(), p.y.to_bits()))
            .collect();
        assert_eq!(distinct.len(), 100, "no two nodes overlap");
    }

    /// Whether any two nodes' boxes overlap, and whether every node lies
    /// inside the reported size.
    fn sound(l: &Layout) -> bool {
        let m = Metrics::default();
        let mut boxes: Vec<(f32, f32)> = l.nodes.iter().map(|p| (p.x, p.y)).collect();
        boxes.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        for (i, a) in boxes.iter().enumerate() {
            if a.0 + m.width > l.width + 0.5 || a.1 + m.height > l.height + 0.5 {
                return false;
            }
            for b in &boxes[i + 1..] {
                if b.0 >= a.0 + m.width - 1e-3 {
                    break;
                }
                if (a.1 - b.1).abs() < m.height - 1e-3 {
                    return false;
                }
            }
        }
        true
    }

    fn names(prefix: &str, k: usize) -> Vec<String> {
        (0..k).map(|i| format!("{prefix}{i:03}")).collect()
    }

    #[test]
    fn a_seed_from_other_metrics_is_ignored_and_bad_metrics_never_panic() {
        let ids = ["a", "b", "c"];
        let edges = [(0, 1), (1, 2)];
        let base = lay(&ids, &edges);
        let seed = base.seed(&ids);
        for m in [
            Metrics {
                width: 0.0,
                height: 0.0,
                gap: 0.0,
                channel: 0.0,
            },
            Metrics {
                width: f32::MAX,
                height: f32::MAX,
                gap: f32::NAN,
                channel: f32::INFINITY,
            },
            Metrics {
                width: 400.0,
                ..Metrics::default()
            },
        ] {
            let l = relayout(&ids, &edges, m, &seed);
            assert_eq!(l.nodes.len(), 3);
        }
        let wide = Metrics {
            width: 400.0,
            ..Metrics::default()
        };
        let l = relayout(&ids, &[(0, 1), (0, 2)], wide, &seed);
        assert!(l.nodes[1].x + 400.0 <= l.nodes[2].x || l.nodes[2].x + 400.0 <= l.nodes[1].x);
    }

    #[test]
    fn relayout_keeps_seeded_grid_cells_clear_of_the_graph() {
        let chain = names("p", 6);
        let loose = names("g", 100);
        let mut ids: Vec<&str> = chain.iter().map(String::as_str).collect();
        ids.extend(loose.iter().map(String::as_str));
        let edges: Vec<(usize, usize)> = (0..5).map(|i| (i, i + 1)).collect();
        let base = lay(&ids, &edges);
        assert!(sound(&base));
        let seed = base.seed(&ids);
        // Keep the chain and one grid node far down the grid; add twenty
        // children of p004.
        let extra = names("q", 20);
        let mut after: Vec<&str> = chain.iter().map(String::as_str).collect();
        after.push("g075");
        after.extend(extra.iter().map(String::as_str));
        let mut e2 = edges.clone();
        e2.extend((0..20).map(|i| (7 + i, 4)));
        let l = relayout(&after, &e2, Metrics::default(), &seed);
        assert!(sound(&l), "{l:?}");
    }

    #[test]
    fn the_reported_size_holds_every_node_after_relayout() {
        let ids = ["a", "b", "c", "d", "e", "z"];
        let base = lay(&ids, &[(0, 1), (1, 2)]);
        let l = relayout(
            &ids,
            &[(0, 1), (1, 2), (1, 3), (1, 4)],
            Metrics::default(),
            &base.seed(&ids),
        );
        assert!(sound(&l), "{l:?}");
    }

    #[test]
    fn deep_graphs_and_wide_relayouts_stay_fast() {
        // A chain with a common sink: every edge to the sink spans the
        // chain, which would need a quadratic number of bends.
        let k = 2000;
        let ids = names("n", k);
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut edges: Vec<(usize, usize)> = (0..k - 2).map(|i| (i, i + 1)).collect();
        edges.extend((0..k - 1).map(|i| (i, k - 1)));
        let t = std::time::Instant::now();
        let l = lay(&ids, &edges);
        assert!(sound(&l));
        // A star relaid from an unrelated seed puts every node in one row.
        let star: Vec<(usize, usize)> = (1..k).map(|i| (i, 0)).collect();
        let r = relayout(&ids, &star, Metrics::default(), &Seed::default());
        assert!(sound(&r));
        assert!(t.elapsed().as_secs() < 20, "took {:?}", t.elapsed());
    }

    #[test]
    fn an_empty_view_has_an_empty_layout() {
        let l = lay(&[], &[]);
        assert!(l.nodes.is_empty() && l.edges.is_empty());
        assert_eq!((l.width, l.height), (0.0, 0.0));
    }
}
