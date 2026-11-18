//! The review list (REQUIREMENTS §5.4, view 1; #90): the branches, tags,
//! pull request refs and recent commits of the open repository, and the
//! base and head of the review. View state only, driven by [`Action`]s, so
//! it is tested headlessly; the window renders it.

use blueprint_protocol::{CommitSummary, Ref, RefKind, Response};

/// One row: a ref or a commit, both leading to a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A branch, tag or pull request ref.
    Ref(Ref),
    /// A recent commit from `HEAD`.
    Commit(CommitSummary),
}

impl Row {
    /// The commit id the row leads to.
    #[must_use]
    pub fn commit(&self) -> &str {
        match self {
            Row::Ref(r) => &r.commit,
            Row::Commit(c) => &c.id,
        }
    }

    /// What the row shows: a ref's short name, or a commit's short id and
    /// summary.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Row::Ref(r) => {
                let short = ["refs/heads/", "refs/remotes/", "refs/tags/", "refs/"]
                    .iter()
                    .find_map(|p| r.name.strip_prefix(p))
                    .unwrap_or(&r.name);
                let kind = match r.kind {
                    RefKind::Branch => "branch",
                    RefKind::RemoteBranch => "remote",
                    RefKind::Tag => "tag",
                    RefKind::PullRequest => "pull request",
                };
                format!("{short}  ({kind})")
            }
            Row::Commit(c) => format!("{}  {}", short(&c.id), c.summary),
        }
    }
}

/// The first ten characters of `id`: enough of a commit id to tell it
/// apart. Safe on any string.
#[must_use]
pub fn short(id: &str) -> &str {
    id.char_indices().nth(10).map_or(id, |(i, _)| &id[..i])
}

/// Something the reviewer does in the list. Each has a key (see
/// [`ACTIONS`]) and a command palette entry (#94).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Move the cursor up one row.
    Up,
    /// Move the cursor down one row.
    Down,
    /// Move the cursor to the first row.
    First,
    /// Move the cursor to the last row.
    Last,
    /// Make the row under the cursor the base.
    PickBase,
    /// Make the row under the cursor the head.
    PickHead,
    /// Swap base and head.
    Swap,
    /// Review base against head.
    Open,
}

/// Every review list action: its name in the command palette and its key.
pub const ACTIONS: &[(Action, &str, &str)] = &[
    (Action::Up, "Review list: previous row", "up"),
    (Action::Down, "Review list: next row", "down"),
    (Action::First, "Review list: first row", "home"),
    (Action::Last, "Review list: last row", "end"),
    (Action::PickBase, "Review list: use as base", "b"),
    (Action::PickHead, "Review list: use as head", "h"),
    (Action::Swap, "Review list: swap base and head", "s"),
    (
        Action::Open,
        "Review list: review base against head",
        "enter",
    ),
];

/// The action bound to `key` (a GPUI key name), if any.
#[must_use]
pub fn action_for_key(key: &str) -> Option<Action> {
    ACTIONS
        .iter()
        .find(|(_, _, k)| *k == key)
        .map(|(a, _, _)| *a)
}

/// What the app should do after an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Nothing beyond redrawing.
    None,
    /// Review `base` against `head` (commit ids): ask the engine for the diff
    /// and show the map.
    Review {
        /// The base commit.
        base: String,
        /// The head commit.
        head: String,
    },
}

/// The review list's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewList {
    /// Refs first (sorted by name), then recent commits, newest first.
    pub rows: Vec<Row>,
    /// The row under the cursor.
    pub cursor: usize,
    /// The base commit, if chosen.
    pub base: Option<String>,
    /// The head commit, if chosen.
    pub head: Option<String>,
    /// The branch `HEAD` is on, for the header.
    pub current: Option<String>,
}

impl ReviewList {
    /// The list for an engine [`Response::Refs`], with base and head set to
    /// the engine's suggestion and the cursor on the head. `None` for any
    /// other response.
    #[must_use]
    pub fn from_response(response: &Response) -> Option<ReviewList> {
        let Response::Refs {
            refs,
            commits,
            current,
            suggested,
        } = response
        else {
            return None;
        };
        let rows: Vec<Row> = refs
            .iter()
            .cloned()
            .map(Row::Ref)
            .chain(commits.iter().cloned().map(Row::Commit))
            .collect();
        let (base, head) = match suggested {
            Some((b, h)) => (Some(b.clone()), Some(h.clone())),
            None => (None, None),
        };
        let cursor = head
            .as_ref()
            .and_then(|h| rows.iter().position(|r| r.commit() == h))
            .unwrap_or(0);
        Some(ReviewList {
            rows,
            cursor,
            base,
            head,
            current: current.clone(),
        })
    }

    /// Applies `action`.
    pub fn apply(&mut self, action: Action) -> Effect {
        let selected = self.rows.get(self.cursor).map(|r| r.commit().to_owned());
        match action {
            Action::Up => self.cursor = self.cursor.saturating_sub(1),
            Action::Down => {
                if self.cursor + 1 < self.rows.len() {
                    self.cursor += 1;
                }
            }
            Action::First => self.cursor = 0,
            Action::Last => self.cursor = self.rows.len().saturating_sub(1),
            Action::PickBase => {
                if selected.is_some() {
                    self.base = selected;
                }
            }
            Action::PickHead => {
                if selected.is_some() {
                    self.head = selected;
                }
            }
            Action::Swap => std::mem::swap(&mut self.base, &mut self.head),
            Action::Open => {
                if let (Some(base), Some(head)) = (&self.base, &self.head) {
                    return Effect::Review {
                        base: base.clone(),
                        head: head.clone(),
                    };
                }
            }
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str) -> CommitSummary {
        CommitSummary {
            id: id.into(),
            summary: format!("commit {id}"),
            author: "A".into(),
            time: 0,
            parents: vec![],
        }
    }

    fn refs() -> Response {
        Response::Refs {
            refs: vec![Ref {
                name: "refs/heads/main".into(),
                kind: RefKind::Branch,
                commit: "c3".into(),
            }],
            commits: vec![commit("c3"), commit("c2"), commit("c1")],
            current: Some("refs/heads/main".into()),
            suggested: Some(("c2".into(), "c3".into())),
        }
    }

    #[test]
    fn starts_from_the_engines_suggestion() {
        let l = ReviewList::from_response(&refs()).unwrap_or_default();
        assert_eq!(l.rows.len(), 4);
        assert_eq!(l.base.as_deref(), Some("c2"));
        assert_eq!(l.head.as_deref(), Some("c3"));
        assert_eq!(l.cursor, 0, "the cursor starts on the head");
        assert_eq!(l.rows[0].label(), "main  (branch)");
    }

    #[test]
    fn two_commits_are_chosen_and_opened_by_keyboard_alone() {
        let mut l = ReviewList::from_response(&refs()).unwrap_or_default();
        for key in ["down", "down", "down", "b", "up", "h"] {
            let a = action_for_key(key);
            assert!(a.is_some(), "{key} is bound");
            if let Some(a) = a {
                assert_eq!(l.apply(a), Effect::None);
            }
        }
        assert_eq!(
            l.apply(Action::Open),
            Effect::Review {
                base: "c1".into(),
                head: "c2".into()
            }
        );
        l.apply(Action::Swap);
        assert_eq!(l.base.as_deref(), Some("c2"));
    }

    #[test]
    fn the_cursor_stays_in_the_list_and_open_needs_both_ends() {
        let mut l = ReviewList::default();
        assert_eq!(l.apply(Action::Up), Effect::None);
        assert_eq!(l.apply(Action::Down), Effect::None);
        assert_eq!(l.apply(Action::PickBase), Effect::None);
        assert_eq!(l.base, None);
        assert_eq!(l.apply(Action::Open), Effect::None);
        assert!(ReviewList::from_response(&Response::Superseded).is_none());
    }

    #[test]
    fn home_and_end_reach_both_ends_and_ids_of_any_text_are_shortened() {
        let mut l = ReviewList::from_response(&refs()).unwrap_or_default();
        l.apply(Action::Last);
        assert_eq!(l.cursor, 3);
        l.apply(Action::PickBase);
        l.apply(Action::First);
        l.apply(Action::PickHead);
        assert_eq!(l.base.as_deref(), Some("c1"));
        assert_eq!(l.head.as_deref(), Some("c3"));
        assert_eq!(short("aaaaaaaaaé"), "aaaaaaaaaé");
        assert_eq!(short("ééééééééééé"), "éééééééééé");
        assert_eq!(short("ab"), "ab");
    }

    #[test]
    fn every_action_has_a_distinct_key() {
        let mut keys: Vec<&str> = ACTIONS.iter().map(|(_, _, k)| *k).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ACTIONS.len());
    }
}
