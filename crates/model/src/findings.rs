//! The findings of a diff in the §3.3 order (#114): new cross-boundary
//! dependencies, then new cycles, then public surface changes, then where
//! the change is concentrated. Shared by the app's findings list and the
//! review export (#116), so both list the same findings the same way.

use crate::diff::{Diff, Finding, Lines, NodeChange};
use crate::{Level, Model, NodeId};

/// How many of the most changed nodes the list ends with.
pub const CONCENTRATION: usize = 10;

/// One finding, as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A new dependency between two systems or two modules.
    Dependency {
        /// The dependent.
        from: NodeId,
        /// What it now depends on.
        to: NodeId,
    },
    /// A new dependency cycle.
    Cycle {
        /// The level, 1 or 2.
        level: u8,
        /// The nodes on the cycle.
        nodes: Vec<NodeId>,
    },
    /// A visible node whose signature or visibility changed.
    Surface {
        /// The node.
        id: NodeId,
    },
    /// One of the nodes whose own lines changed most.
    Concentration {
        /// The node.
        id: NodeId,
        /// Its own changed lines: its rolled-up weight less its children's.
        lines: Lines,
    },
}

impl Item {
    /// The node Enter shows on the map: the dependent of a dependency, the
    /// first node of a cycle, the node itself otherwise.
    #[must_use]
    pub fn target(&self) -> Option<&NodeId> {
        match self {
            Item::Dependency { from, .. } => Some(from),
            Item::Cycle { nodes, .. } => nodes.first(),
            Item::Surface { id } | Item::Concentration { id, .. } => Some(id),
        }
    }
}

/// A node's parent, from head or else base.
fn parent(id: &NodeId, base: &Model, head: &Model) -> Option<NodeId> {
    head.node(id)
        .or_else(|| base.node(id))
        .and_then(|n| n.parent.clone())
}

/// A node's name, from head or else base; its id if neither has it.
#[must_use]
pub fn name(id: &NodeId, base: &Model, head: &Model) -> String {
    head.node(id)
        .or_else(|| base.node(id))
        .map_or_else(|| id.to_string(), |n| n.name.clone())
}

/// The findings of `diff` between `base` and `head`, in the §3.3 order.
#[must_use]
pub fn items(diff: &Diff, base: &Model, head: &Model) -> Vec<Item> {
    let mut deps = Vec::new();
    let mut cycles = Vec::new();
    // One row per pair of nodes, whatever kinds of edge join them.
    let mut pairs = std::collections::HashSet::new();
    for f in &diff.findings {
        match f {
            Finding::NewDependency { edge } => {
                if pairs.insert((edge.from.clone(), edge.to.clone())) {
                    deps.push(Item::Dependency {
                        from: edge.from.clone(),
                        to: edge.to.clone(),
                    });
                }
            }
            Finding::NewCycle { level, nodes, .. } => cycles.push(Item::Cycle {
                level: *level,
                nodes: nodes.clone(),
            }),
        }
    }
    let surface = diff.nodes.iter().filter_map(|c| match c {
        NodeChange::Surface { id } => Some(Item::Surface { id: id.clone() }),
        _ => None,
    });
    // Concentration: weights roll up into every ancestor, so ranking
    // them would list one change at each level. Rank each node by its
    // own lines (its weight less its children's) instead, below L1.
    let mut own: std::collections::BTreeMap<NodeId, (u32, u32)> = diff
        .weights
        .iter()
        .map(|w| (w.id.clone(), (w.lines.signature, w.lines.body)))
        .collect();
    for w in &diff.weights {
        if let Some(p) = parent(&w.id, base, head)
            && let Some(o) = own.get_mut(&p)
        {
            o.0 = o.0.saturating_sub(w.lines.signature);
            o.1 = o.1.saturating_sub(w.lines.body);
        }
    }
    let mut heavy: Vec<(NodeId, Lines)> = own
        .into_iter()
        .filter(|(id, (s, b))| id.level() != Level::System && s + b > 0)
        .map(|(id, (signature, body))| (id, Lines { signature, body }))
        .collect();
    heavy.sort_by(|a, b| {
        let total = |l: Lines| l.signature + l.body;
        total(b.1).cmp(&total(a.1)).then_with(|| a.0.cmp(&b.0))
    });
    let concentration = heavy
        .into_iter()
        .take(CONCENTRATION)
        .map(|(id, lines)| Item::Concentration { id, lines });
    deps.into_iter()
        .chain(cycles)
        .chain(surface)
        .chain(concentration)
        .collect()
}

/// A node's name, after its system's for anything below L1 ("a › x"), so
/// that same-named modules of different systems tell apart.
pub fn qualified(id: &NodeId, base: &Model, head: &Model) -> String {
    if id.level() == Level::System {
        return name(id, base, head);
    }
    let system = NodeId::system(id.system_key().clone());
    format!("{} › {}", name(&system, base, head), name(id, base, head))
}

/// A finding's text, with node names from `base` and `head`.
#[must_use]
pub fn label(item: &Item, base: &Model, head: &Model) -> String {
    match item {
        Item::Dependency { from, to } => {
            format!(
                "New dependency: {} → {}",
                qualified(from, base, head),
                qualified(to, base, head)
            )
        }
        Item::Cycle { level, nodes } => {
            let names: Vec<String> = nodes.iter().map(|n| qualified(n, base, head)).collect();
            format!("New cycle at L{level}: {}", names.join(" → "))
        }
        Item::Surface { id } => format!("Public surface changed: {}", qualified(id, base, head)),
        Item::Concentration { id, lines } => format!(
            "Change concentrated in {}: {} lines ({} in signatures)",
            qualified(id, base, head),
            lines.signature + lines.body,
            lines.signature
        ),
    }
}
