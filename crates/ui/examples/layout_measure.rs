//! Measures the map layout on a real repository for DR 0010 (#91): layout
//! time of the largest views, and how many nodes move after small changes.
//!
//! ```text
//! cargo run --release -p blueprint-ui --example layout_measure -- <repo> [rev]
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use blueprint_engine::commits;
use blueprint_model::{Model, NodeId};
use blueprint_ui::layout::{Layout, Metrics, Seed, layout, relayout};

/// The views of `model`: each node's children (and the systems, under
/// `None`), with the model's edges rolled up to the pairs of siblings
/// whose subtrees they join.
/// Each view: its parent, its nodes (sorted) and its edges.
type Views = BTreeMap<Option<NodeId>, (Vec<NodeId>, Vec<(usize, usize)>)>;

fn views(model: &Model) -> Views {
    let mut out = Views::new();
    for n in model.nodes() {
        out.entry(n.parent.clone())
            .or_default()
            .0
            .push(n.id.clone());
    }
    for v in out.values_mut() {
        v.0.sort();
    }
    let chain = |id: &NodeId| {
        let mut c = vec![id.clone()];
        let mut at = id.clone();
        while let Some(p) = model.node(&at).and_then(|n| n.parent.clone()) {
            c.push(p.clone());
            at = p;
        }
        c.reverse();
        c
    };
    for e in model.edges() {
        let (a, b) = (chain(&e.key.from), chain(&e.key.to));
        let Some(i) = (0..a.len().min(b.len())).find(|i| a[*i] != b[*i]) else {
            continue;
        };
        let parent = if i == 0 { None } else { Some(a[i - 1].clone()) };
        let Some(view) = out.get_mut(&parent) else {
            continue;
        };
        let (Ok(x), Ok(y)) = (view.0.binary_search(&a[i]), view.0.binary_search(&b[i])) else {
            continue;
        };
        view.1.push((x, y));
    }
    for v in out.values_mut() {
        v.1.sort_unstable();
        v.1.dedup();
    }
    out
}

fn best_ms(ids: &[&str], edges: &[(usize, usize)], seed: Option<&Seed>) -> (f64, Layout) {
    let mut best = f64::INFINITY;
    let mut l = Layout::default();
    for _ in 0..5 {
        let t = Instant::now();
        l = match seed {
            Some(s) => relayout(ids, edges, Metrics::default(), s),
            None => layout(ids, edges, Metrics::default()),
        };
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    (best, l)
}

/// How many of `ids` sit somewhere else in `after`, which laid out
/// `after_ids`.
fn moved(ids: &[&str], before: &Layout, after_ids: &[&str], after: &Layout) -> usize {
    let at: BTreeMap<&str, (u32, u32)> = after_ids
        .iter()
        .zip(&after.nodes)
        .map(|(i, p)| (*i, (p.x.to_bits(), p.y.to_bits())))
        .collect();
    ids.iter()
        .zip(&before.nodes)
        .filter(|(i, p)| {
            at.get(*i)
                .is_some_and(|q| *q != (p.x.to_bits(), p.y.to_bits()))
        })
        .count()
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let repo = Path::new(args.first().ok_or("usage: layout_measure <repo> [rev]")?);
    let rev = args.get(1).map_or("HEAD", String::as_str);
    let (index, _) = commits::existing_index(repo);
    let (model, _) = commits::l4_at(repo, rev, index.as_ref())?;
    let views = views(&model);
    let mut by_size: Vec<_> = views.iter().collect();
    by_size.sort_by_key(|(k, v)| (std::cmp::Reverse(v.0.len()), (*k).clone()));
    let largest_l2 = views
        .iter()
        .filter(|(k, _)| {
            k.as_ref()
                .is_some_and(|k| model.node(k).is_some_and(|n| n.parent.is_none()))
        })
        .max_by_key(|(_, v)| v.0.len());
    let largest_with_edges = by_size.iter().find(|(_, v)| !v.1.is_empty()).copied();
    let mut pick: Vec<(&str, &Option<NodeId>)> = vec![("L1 (systems)", &None)];
    if let Some((k, _)) = largest_l2 {
        pick.push(("largest L2 view", k));
    }
    if let Some((k, _)) = largest_with_edges {
        pick.push(("largest view with edges", k));
    }
    if let Some((k, _)) = by_size.first() {
        pick.push(("largest view", k));
    }
    println!(
        "| view | parent | nodes | edges | layout ms | relayout ms | moved, fresh layout: new node / new edge / edge removed | moved, relayout: new node / new edge / edge removed |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    for (label, key) in pick {
        let (nodes, edges) = &views[key];
        let names: Vec<String> = nodes.iter().map(ToString::to_string).collect();
        let ids: Vec<&str> = names.iter().map(String::as_str).collect();
        let (ms, base) = best_ms(&ids, edges, None);
        let seed = base.seed(&ids);
        // A one-file change seen from one view: a node added with an
        // edge, an edge added, or an edge removed.
        let mut added_ids = ids.clone();
        added_ids.push("~new");
        let mut added_edges = edges.clone();
        if !ids.is_empty() {
            added_edges.push((ids.len(), 0));
        }
        let mut more = edges.clone();
        if ids.len() > 2 {
            more.push((ids.len() / 3, 2 * ids.len() / 3));
        }
        let mut fewer = edges.clone();
        if !fewer.is_empty() {
            fewer.remove(fewer.len() / 2);
        }
        let pct = |k: usize| {
            if ids.is_empty() {
                "0".to_string()
            } else {
                format!("{:.0}%", 100.0 * k as f64 / ids.len() as f64)
            }
        };
        let mut cols = Vec::new();
        let mut relayout_ms = 0.0f64;
        for s in [None, Some(&seed)] {
            let (t1, with_node) = best_ms(&added_ids, &added_edges, s);
            let (t2, with_edge) = best_ms(&ids, &more, s);
            let (t3, without_edge) = best_ms(&ids, &fewer, s);
            if s.is_some() {
                relayout_ms = t1.max(t2).max(t3);
            }
            cols.push(format!(
                "{} / {} / {}",
                pct(moved(&ids, &base, &added_ids, &with_node)),
                pct(moved(&ids, &base, &ids, &with_edge)),
                pct(moved(&ids, &base, &ids, &without_edge)),
            ));
        }
        println!(
            "| {label} | {} | {} | {} | {ms:.2} | {relayout_ms:.2} | {} | {} |",
            key.as_ref().map_or("—".to_string(), ToString::to_string),
            ids.len(),
            edges.len(),
            cols[0],
            cols[1],
        );
    }
    Ok(())
}
