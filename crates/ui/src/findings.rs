//! The findings view (#114, REQUIREMENTS §5.4 view 3): the review's
//! starting point, in the §3.3 order. New cross-boundary dependencies first,
//! then new cycles, then public surface changes, then where the change is
//! concentrated. Each row leads to a node on the map.

use blueprint_protocol::model::diff::{Diff, Finding, Lines, NodeChange};
use blueprint_protocol::model::{Level, NodeId};

use crate::map::Map;

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
    /// One of the most changed modules or below.
    Concentration {
        /// The node.
        id: NodeId,
        /// Its changed lines, its descendants' included.
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

/// The findings list's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Findings {
    /// The findings, in order.
    pub items: Vec<Item>,
    /// The highlighted row.
    pub cursor: usize,
}

impl Findings {
    /// The findings of `diff`, in the §3.3 order.
    #[must_use]
    pub fn new(diff: &Diff) -> Findings {
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
        // Concentration: modules and below, most changed lines first; the
        // systems would always win, as their weights hold everything.
        let mut heavy: Vec<_> = diff
            .weights
            .iter()
            .filter(|w| w.id.level() != Level::System && !w.lines.is_zero())
            .collect();
        heavy.sort_by(|a, b| {
            let total = |l: Lines| l.signature + l.body;
            total(b.lines)
                .cmp(&total(a.lines))
                .then_with(|| a.id.cmp(&b.id))
        });
        let concentration = heavy
            .into_iter()
            .take(CONCENTRATION)
            .map(|w| Item::Concentration {
                id: w.id.clone(),
                lines: w.lines,
            });
        let items = deps
            .into_iter()
            .chain(cycles)
            .chain(surface)
            .chain(concentration)
            .collect();
        Findings { items, cursor: 0 }
    }

    /// The highlighted finding.
    #[must_use]
    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.cursor)
    }

    /// Moves the cursor by `by` rows, within the list.
    pub fn move_by(&mut self, by: isize) {
        let last = self.items.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(by).min(last);
    }

    /// Moves the cursor to the first (`end` false) or last row.
    pub fn to_end(&mut self, end: bool) {
        self.cursor = if end {
            self.items.len().saturating_sub(1)
        } else {
            0
        };
    }
}

/// A node's name, after its system's for anything below L1 ("a › x"), so
/// that same-named modules of different systems tell apart.
fn qualified(id: &NodeId, map: &Map) -> String {
    if id.level() == Level::System {
        return map.name(id);
    }
    let system = NodeId::system(id.system_key().clone());
    format!("{} › {}", map.name(&system), map.name(id))
}

/// A row's text, with node names from `map`.
#[must_use]
pub fn label(item: &Item, map: &Map) -> String {
    match item {
        Item::Dependency { from, to } => {
            format!(
                "New dependency: {} → {}",
                qualified(from, map),
                qualified(to, map)
            )
        }
        Item::Cycle { level, nodes } => {
            let names: Vec<String> = nodes.iter().map(|n| qualified(n, map)).collect();
            format!("New cycle at L{level}: {}", names.join(" → "))
        }
        Item::Surface { id } => format!("Public surface changed: {}", qualified(id, map)),
        Item::Concentration { id, lines } => format!(
            "Change concentrated in {}: {} lines ({} in signatures)",
            qualified(id, map),
            lines.signature + lines.body,
            lines.signature
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use blueprint_engine::analyse_deep;
    use blueprint_protocol::model::{Model, RepoPath};

    fn model(files: &[(&str, &str)]) -> Model {
        let map: BTreeMap<RepoPath, Vec<u8>> = files
            .iter()
            .filter_map(|(p, b)| Some((RepoPath::parse(p).ok()?, b.as_bytes().to_vec())))
            .collect();
        let paths: Vec<RepoPath> = map.keys().cloned().collect();
        analyse_deep(&paths, &|p| map.get(p).cloned()).0
    }

    const WS: (&str, &str) = ("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\n");

    const BASE: &[(&str, &str)] = &[
        WS,
        ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
        (
            "a/src/lib.rs",
            "pub mod x;\npub fn f() -> u32 {\n    1\n}\n",
        ),
        ("a/src/x.rs", "pub fn g() {}\n"),
        ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ("b/src/lib.rs", "pub fn h() {}\n"),
    ];

    /// b gains a dependency on a, a's `f` changes signature and a's `g` its
    /// body.
    const HEAD: &[(&str, &str)] = &[
        WS,
        ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
        (
            "a/src/lib.rs",
            "pub mod x;\npub fn f() -> u64 {\n    1\n}\n",
        ),
        ("a/src/x.rs", "pub fn g() {\n    let _ = 1;\n}\n"),
        (
            "b/Cargo.toml",
            "[package]\nname = \"b\"\n\n[dependencies]\na = { path = \"../a\" }\n",
        ),
        ("b/src/lib.rs", "pub fn h() {\n    a::f();\n}\n"),
    ];

    fn review() -> (Map, Findings) {
        let (base, head) = (model(BASE), model(HEAD));
        let mut diff = blueprint_engine::diff::diff(&base, &head);
        let paths: Vec<(RepoPath, &str, &str)> = BASE
            .iter()
            .zip(HEAD)
            .filter(|(b, h)| b.1 != h.1)
            .filter_map(|(b, h)| Some((RepoPath::parse(b.0).ok()?, b.1, h.1)))
            .collect();
        let files: Vec<blueprint_engine::diff::ChangedFile<'_>> = paths
            .iter()
            .map(|(p, b, h)| blueprint_engine::diff::ChangedFile {
                path: p,
                base: Some(b.as_bytes()),
                head: Some(h.as_bytes()),
            })
            .collect();
        blueprint_engine::diff::weigh(&mut diff, &base, &head, &files);
        let f = Findings::new(&diff);
        (Map::new(Arc::new(base), Arc::new(head), &diff), f)
    }

    #[test]
    fn findings_come_in_the_section_3_3_order() {
        let (map, f) = review();
        let rank = |i: &Item| match i {
            Item::Dependency { .. } => 0,
            Item::Cycle { .. } => 1,
            Item::Surface { .. } => 2,
            Item::Concentration { .. } => 3,
        };
        let ranks: Vec<u8> = f.items.iter().map(rank).collect();
        assert!(ranks.windows(2).all(|w| w[0] <= w[1]), "{ranks:?}");
        let labels: Vec<String> = f.items.iter().map(|i| label(i, &map)).collect();
        assert!(
            labels.iter().any(|l| l == "New dependency: b → a"),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "Public surface changed: a › f"),
            "{labels:?}"
        );
        assert!(
            f.items.iter().all(|i| match i {
                Item::Concentration { id, .. } => id.level() != Level::System,
                _ => true,
            }),
            "systems are not concentration findings"
        );
    }

    #[test]
    fn each_finding_leads_to_its_node_on_the_map() {
        let (mut map, f) = review();
        for item in &f.items {
            let Some(target) = item.target() else {
                panic!("{item:?} has no target");
            };
            assert!(map.reveal(target), "{item:?}");
            let shown = map
                .selected
                .and_then(|i| map.view.nodes.get(i))
                .map(|n| &n.id);
            assert_eq!(shown, Some(target));
        }
    }

    #[test]
    fn the_cursor_stays_in_the_list() {
        let (_, mut f) = review();
        f.move_by(-3);
        assert_eq!(f.cursor, 0);
        f.move_by(1000);
        assert_eq!(f.cursor, f.items.len() - 1);
        f.to_end(false);
        assert_eq!(f.cursor, 0);
        let mut empty = Findings::default();
        empty.move_by(1);
        assert_eq!(empty.cursor, 0);
        assert!(empty.current().is_none());
    }
}
