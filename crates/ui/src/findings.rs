//! The findings view (#114, REQUIREMENTS §5.4 view 3): the review's
//! starting point, in the §3.3 order. New cross-boundary dependencies first,
//! then new cycles, then public surface changes, then where the change is
//! concentrated. Each row leads to a node on the map.

use blueprint_protocol::model::diff::Diff;
pub use blueprint_protocol::model::findings::{CONCENTRATION, Item};

use crate::map::Map;

/// The findings list's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Findings {
    /// The findings, in order.
    pub items: Vec<Item>,
    /// The highlighted row.
    pub cursor: usize,
}

impl Findings {
    /// The findings of `diff`, in the §3.3 order; `map` is the map of the
    /// same diff, for each node's parent.
    #[must_use]
    pub fn new(diff: &Diff, map: &Map) -> Findings {
        Findings {
            items: blueprint_protocol::model::findings::items(diff, map.base(), map.head()),
            cursor: 0,
        }
    }

    /// Highlights `item` if the list has it, else the row at `cursor`,
    /// within the list: keeping a place across a rebuilt list.
    pub fn keep(&mut self, item: Option<&Item>, cursor: usize) {
        self.cursor = item
            .and_then(|it| self.items.iter().position(|i| i == it))
            .unwrap_or_else(|| cursor.min(self.items.len().saturating_sub(1)));
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

/// A row's text, with node names from `map`.
#[must_use]
pub fn label(item: &Item, map: &Map) -> String {
    blueprint_protocol::model::findings::label(item, map.base(), map.head())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use blueprint_engine::analyse_deep;
    use blueprint_protocol::model::{Level, Model, NodeId, RepoPath};

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
        let map = Map::new(Arc::new(base), Arc::new(head), &diff);
        let f = Findings::new(&diff, &map);
        (map, f)
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
    fn concentration_counts_each_line_once() {
        let (map, f) = review();
        let rows: Vec<String> = f
            .items
            .iter()
            .filter(|i| matches!(i, Item::Concentration { .. }))
            .map(|i| label(i, &map))
            .collect();
        // g's body grew by two lines: only g carries them, not x above it.
        assert!(rows.iter().any(|r| r.contains("› g:")), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("› x:")), "{rows:?}");
    }

    #[test]
    fn a_rebuilt_list_keeps_the_highlighted_finding() {
        let (_, mut f) = review();
        f.to_end(true);
        let last = f.current().cloned();
        let mut again = f.clone();
        again.items.insert(
            0,
            Item::Surface {
                id: NodeId::system(
                    last.as_ref()
                        .and_then(Item::target)
                        .map(|t| t.system_key().clone())
                        .unwrap_or_else(|| panic!("a target")),
                ),
            },
        );
        again.keep(last.as_ref(), 0);
        assert_eq!(again.current(), last.as_ref());
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
