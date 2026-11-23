//! The one registry of actions (#94, REQUIREMENTS §2.4 and §5.4 view 6).
//!
//! Every action the app has is listed in [`ACTIONS`] with its palette title,
//! the place it applies (its [`Scope`]) and its keys. The workspace turns a
//! key into an action through [`for_key`] and runs actions by id, and the
//! command palette searches the same list, so an action cannot exist without
//! a key and a palette entry.

/// Where an action applies. A screen has several scopes, searched in order,
/// so a narrower scope's key wins over a wider one's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The review list.
    List,
    /// The findings list of a review (#114).
    Findings,
    /// The map of a review.
    Map,
    /// The outline tree of a review (#117).
    Outline,
    /// The code diff of one file.
    Code,
    /// Any review: the map, the code, or one that failed.
    Review,
    /// Everywhere.
    Global,
}

/// One action: what the palette shows and the keys that run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionDef {
    /// Stable identifier, `scope.verb`.
    pub id: &'static str,
    /// The palette entry.
    pub title: &'static str,
    /// Where it applies.
    pub scope: Scope,
    /// GPUI key names, modifiers first (`ctrl-shift-p`); the first is shown
    /// in the palette.
    pub keys: &'static [&'static str],
}

const fn a(
    id: &'static str,
    title: &'static str,
    scope: Scope,
    keys: &'static [&'static str],
) -> ActionDef {
    ActionDef {
        id,
        title,
        scope,
        keys,
    }
}

/// Every action of the app.
pub const ACTIONS: &[ActionDef] = &[
    a(
        "global.palette",
        "Command palette",
        Scope::Global,
        &["f1", "ctrl-shift-p", "cmd-shift-p"],
    ),
    a(
        "global.open",
        "Open a repository…",
        Scope::Global,
        &["ctrl-o", "cmd-o"],
    ),
    a("list.up", "Review list: previous row", Scope::List, &["up"]),
    a("list.down", "Review list: next row", Scope::List, &["down"]),
    a(
        "list.first",
        "Review list: first row",
        Scope::List,
        &["home"],
    ),
    a("list.last", "Review list: last row", Scope::List, &["end"]),
    a("list.base", "Review list: use as base", Scope::List, &["b"]),
    a("list.head", "Review list: use as head", Scope::List, &["h"]),
    a(
        "list.swap",
        "Review list: swap base and head",
        Scope::List,
        &["s"],
    ),
    a(
        "list.open",
        "Review list: review base against head",
        Scope::List,
        &["enter"],
    ),
    a(
        "findings.up",
        "Findings: previous finding",
        Scope::Findings,
        &["up"],
    ),
    a(
        "findings.down",
        "Findings: next finding",
        Scope::Findings,
        &["down"],
    ),
    a(
        "findings.first",
        "Findings: first finding",
        Scope::Findings,
        &["home"],
    ),
    a(
        "findings.last",
        "Findings: last finding",
        Scope::Findings,
        &["end"],
    ),
    a(
        "findings.show",
        "Findings: show the finding on the map",
        Scope::Findings,
        &["enter"],
    ),
    a(
        "findings.map",
        "Findings: open the map at the systems",
        Scope::Findings,
        &["m"],
    ),
    a(
        "map.outline",
        "Map: the outline tree (the map as a list)",
        Scope::Map,
        &["o"],
    ),
    a(
        "outline.up",
        "Outline: previous row",
        Scope::Outline,
        &["up"],
    ),
    a(
        "outline.down",
        "Outline: next row",
        Scope::Outline,
        &["down"],
    ),
    a(
        "outline.first",
        "Outline: first row",
        Scope::Outline,
        &["home"],
    ),
    a(
        "outline.last",
        "Outline: last row",
        Scope::Outline,
        &["end"],
    ),
    a(
        "outline.expand",
        "Outline: expand, or go to the first child",
        Scope::Outline,
        &["right"],
    ),
    a(
        "outline.collapse",
        "Outline: collapse, or go to the parent",
        Scope::Outline,
        &["left"],
    ),
    a(
        "outline.show",
        "Outline: show the node on the map",
        Scope::Outline,
        &["enter"],
    ),
    a(
        "outline.map",
        "Outline: back to the map, at the highlighted node",
        Scope::Outline,
        &["o", "escape"],
    ),
    a(
        "map.findings",
        "Map: back to the findings",
        Scope::Map,
        &["f"],
    ),
    a(
        "map.drill_down",
        "Map: drill into the selected node (or open its code)",
        Scope::Map,
        &["enter"],
    ),
    a(
        "map.drill_up",
        "Map: up one level",
        Scope::Map,
        &["backspace"],
    ),
    a(
        "map.code",
        "Map: open the selected node's code",
        Scope::Map,
        &["c"],
    ),
    a("map.next", "Map: next node", Scope::Map, &["tab"]),
    a(
        "map.left",
        "Map: select the node to the left",
        Scope::Map,
        &["left"],
    ),
    a(
        "map.right",
        "Map: select the node to the right",
        Scope::Map,
        &["right"],
    ),
    a("map.up", "Map: select the node above", Scope::Map, &["up"]),
    a(
        "map.down",
        "Map: select the node below",
        Scope::Map,
        &["down"],
    ),
    a(
        "map.pan_left",
        "Map: pan left",
        Scope::Map,
        &["ctrl-left", "cmd-left"],
    ),
    a(
        "map.pan_right",
        "Map: pan right",
        Scope::Map,
        &["ctrl-right", "cmd-right"],
    ),
    a(
        "map.pan_up",
        "Map: pan up",
        Scope::Map,
        &["ctrl-up", "cmd-up"],
    ),
    a(
        "map.pan_down",
        "Map: pan down",
        Scope::Map,
        &["ctrl-down", "cmd-down"],
    ),
    a(
        "map.zoom_in",
        "Map: zoom in",
        Scope::Map,
        &["=", "+", "ctrl-=", "cmd-=", "add"],
    ),
    a(
        "map.zoom_out",
        "Map: zoom out",
        Scope::Map,
        &["-", "ctrl--", "cmd--", "subtract"],
    ),
    a(
        "map.zoom_reset",
        "Map: reset zoom and pan",
        Scope::Map,
        &["0"],
    ),
    a(
        "code.close",
        "Code: back to the map",
        Scope::Code,
        &["escape", "backspace"],
    ),
    a("code.up", "Code: previous row", Scope::Code, &["up"]),
    a("code.down", "Code: next row", Scope::Code, &["down"]),
    a("code.page_up", "Code: page up", Scope::Code, &["pageup"]),
    a(
        "code.page_down",
        "Code: page down",
        Scope::Code,
        &["pagedown"],
    ),
    a("code.first", "Code: first row", Scope::Code, &["home"]),
    a("code.last", "Code: last row", Scope::Code, &["end"]),
    a("code.next_change", "Code: next change", Scope::Code, &["n"]),
    a(
        "code.previous_change",
        "Code: previous change",
        Scope::Code,
        &["p"],
    ),
    a(
        "code.scroll_left",
        "Code: scroll left",
        Scope::Code,
        &["left"],
    ),
    a(
        "code.scroll_right",
        "Code: scroll right",
        Scope::Code,
        &["right"],
    ),
    a(
        "review.back",
        "Review: back to the review list",
        Scope::Review,
        &["escape"],
    ),
    a(
        "review.export",
        "Review: copy the review as Markdown (for a pull request comment)",
        Scope::Review,
        &["x"],
    ),
    a(
        "review.note",
        "Review: write a note on the node (the selected node, the finding's, or the code's)",
        Scope::Review,
        &["e"],
    ),
    a(
        "review.reviewed",
        "Review: mark reviewed, or not (the selected node, the finding's, or the code's)",
        Scope::Review,
        &["r"],
    ),
];

/// The action with this id.
#[must_use]
pub fn by_id(id: &str) -> Option<&'static ActionDef> {
    ACTIONS.iter().find(|a| a.id == id)
}

/// The action `key` runs in `scopes` (narrowest first), if any.
#[must_use]
pub fn for_key(scopes: &[Scope], key: &str) -> Option<&'static ActionDef> {
    scopes.iter().find_map(|s| {
        ACTIONS
            .iter()
            .find(|a| a.scope == *s && a.keys.contains(&key))
    })
}

/// The actions available in `scopes` whose title matches `query`, best
/// first. Each query character must appear in the title in order, ignoring
/// case; a match at the start of the title or of a word ranks higher. An
/// action shadowed by a narrower scope's action on all of its keys is still
/// listed: the palette runs actions by id, not by key.
#[must_use]
pub fn search(scopes: &[Scope], query: &str) -> Vec<&'static ActionDef> {
    let phrase = query.trim().to_lowercase();
    let q: Vec<char> = phrase.chars().filter(|c| !c.is_whitespace()).collect();
    let mut hits: Vec<((u8, u32), usize, &'static ActionDef)> = ACTIONS
        .iter()
        .enumerate()
        .filter(|(_, a)| scopes.contains(&a.scope) && a.id != "global.palette")
        .filter_map(|(i, a)| score(a.title, &phrase, &q).map(|s| (s, i, a)))
        .collect();
    // Titles holding the query as typed first, then scope order (narrowest
    // first), then score, then registry order.
    hits.sort_by_key(|((tier, s), i, a)| {
        let rank = scopes
            .iter()
            .position(|x| *x == a.scope)
            .unwrap_or(usize::MAX);
        (std::cmp::Reverse(*tier), rank, std::cmp::Reverse(*s), *i)
    });
    hits.into_iter().map(|(_, _, a)| a).collect()
}

/// How well `title` matches: `(tier, score)`, or `None`. Tier 2 when the
/// title holds `phrase` as typed (score higher at a word start), tier 1
/// when it holds the characters `q` in order (score higher for word starts
/// and runs).
fn score(title: &str, phrase: &str, q: &[char]) -> Option<(u8, u32)> {
    let t = title.to_lowercase();
    if !phrase.is_empty()
        && let Some(found) = t
            .match_indices(phrase)
            .map(|(i, _)| i)
            .find(|&i| i == 0 || !t[..i].ends_with(char::is_alphanumeric))
            .or_else(|| t.find(phrase))
    {
        let word_start = found == 0 || !t[..found].ends_with(char::is_alphanumeric);
        return Some((2, u32::from(word_start)));
    }
    let t: Vec<char> = t.chars().collect();
    let mut score = 0u32;
    let mut at = 0usize;
    for &c in q {
        let found = t.get(at..)?.iter().position(|&x| x == c)? + at;
        let word_start = found == 0 || !t[found - 1].is_alphanumeric();
        score += if word_start {
            3
        } else if found == at && at > 0 {
            2
        } else {
            0
        };
        at = found + 1;
    }
    Some((1, score))
}

/// The command palette's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// The scopes it was opened in.
    pub scopes: Vec<Scope>,
    /// What has been typed.
    pub query: String,
    /// The matching actions, best first.
    pub matches: Vec<&'static ActionDef>,
    /// The highlighted match.
    pub cursor: usize,
}

/// What a key in the palette leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteEffect {
    /// Keep the palette open.
    Stay,
    /// Close it without running anything.
    Close,
    /// Close it and run this action.
    Run(&'static ActionDef),
}

impl Palette {
    /// A palette listing every action of `scopes`.
    #[must_use]
    pub fn new(scopes: Vec<Scope>) -> Self {
        let matches = search(&scopes, "");
        Palette {
            scopes,
            query: String::new(),
            matches,
            cursor: 0,
        }
    }

    /// Follows the screen under the palette: lists the actions of `scopes`
    /// if they changed since it opened (a map arrived, a review failed).
    pub fn rescope(&mut self, scopes: Vec<Scope>) {
        if scopes != self.scopes {
            self.scopes = scopes;
            self.matches = search(&self.scopes, &self.query);
            self.cursor = self.cursor.min(self.matches.len().saturating_sub(1));
        }
    }

    /// Applies one key (a GPUI key name).
    pub fn key(&mut self, key: &str) -> PaletteEffect {
        if by_id("global.palette").is_some_and(|a| a.keys.contains(&key)) {
            return PaletteEffect::Close;
        }
        match key {
            "escape" => return PaletteEffect::Close,
            "enter" => {
                return self
                    .matches
                    .get(self.cursor)
                    .map_or(PaletteEffect::Stay, |a| PaletteEffect::Run(a));
            }
            "up" => self.cursor = self.cursor.saturating_sub(1),
            "down" => {
                self.cursor = (self.cursor + 1).min(self.matches.len().saturating_sub(1));
            }
            "backspace" => {
                self.query.pop();
                self.refresh();
            }
            "space" => {
                self.query.push(' ');
                self.refresh();
            }
            k if k.chars().count() == 1 => {
                self.query.push_str(k);
                self.refresh();
            }
            _ => {}
        }
        PaletteEffect::Stay
    }

    fn refresh(&mut self) {
        self.matches = search(&self.scopes, &self.query);
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const ALL: [Scope; 5] = [
        Scope::List,
        Scope::Map,
        Scope::Code,
        Scope::Review,
        Scope::Global,
    ];

    #[test]
    fn every_action_has_a_key_a_palette_entry_and_a_unique_id() {
        let mut ids = HashSet::new();
        for a in ACTIONS {
            assert!(ids.insert(a.id), "{} twice", a.id);
            assert!(!a.keys.is_empty(), "{} has no key", a.id);
            assert!(!a.title.is_empty(), "{} has no title", a.id);
            // Reachable by key in its own scope...
            assert!(
                a.keys.iter().any(|k| for_key(&[a.scope], k) == Some(a)),
                "{} is shadowed in its own scope",
                a.id
            );
            // ...and listed by the palette under its title.
            if a.id != "global.palette" {
                assert!(search(&[a.scope], a.title).contains(&a), "{}", a.id);
            }
        }
    }

    #[test]
    fn every_key_is_a_gpui_key_name() {
        for a in ACTIONS {
            for k in a.keys {
                let parsed = gpui::Keystroke::parse(k);
                assert!(parsed.is_ok(), "{k} of {} does not parse", a.id);
                if let Ok(p) = parsed {
                    assert!(
                        k.ends_with(p.key.as_str())
                            && (p.key.chars().count() == 1
                                || p.key
                                    .chars()
                                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())),
                        "{k} of {} is not a GPUI key name",
                        a.id
                    );
                }
            }
        }
    }

    #[test]
    fn the_palette_follows_the_screen_and_its_key_closes_it() {
        let mut p = Palette::new(vec![Scope::Review, Scope::Global]);
        assert_eq!(
            p.matches.len(),
            5,
            "back to the list, export, note, mark reviewed, open a repository"
        );
        p.rescope(vec![Scope::Map, Scope::Review, Scope::Global]);
        assert!(p.matches.iter().any(|a| a.id == "map.drill_down"));
        assert_eq!(p.key("f1"), PaletteEffect::Close);
    }

    #[test]
    fn no_key_is_bound_twice_in_one_scope() {
        for s in ALL {
            let mut seen = HashSet::new();
            for a in ACTIONS.iter().filter(|a| a.scope == s) {
                for k in a.keys {
                    assert!(seen.insert(*k), "{k} twice in {s:?}");
                }
            }
        }
    }

    #[test]
    fn narrower_scopes_win() {
        let code = [Scope::Code, Scope::Review, Scope::Global];
        assert_eq!(for_key(&code, "escape").map(|a| a.id), Some("code.close"));
        let map = [Scope::Map, Scope::Review, Scope::Global];
        assert_eq!(for_key(&map, "escape").map(|a| a.id), Some("review.back"));
        assert_eq!(for_key(&map, "f1").map(|a| a.id), Some("global.palette"));
        assert_eq!(for_key(&map, "n"), None);
    }

    #[test]
    fn the_palette_filters_moves_and_runs() {
        let mut p = Palette::new(vec![Scope::List, Scope::Global]);
        assert_eq!(
            p.matches.len(),
            9,
            "every list action, not the palette itself"
        );
        for k in ["s", "w", "a", "p"] {
            assert_eq!(p.key(k), PaletteEffect::Stay);
        }
        assert_eq!(p.matches.first().map(|a| a.id), Some("list.swap"));
        assert_eq!(
            p.key("enter"),
            PaletteEffect::Run(by_id("list.swap").unwrap_or(&ACTIONS[0]))
        );
        p.key("backspace");
        assert_eq!(p.query, "swa");
        for k in ["z", "z", "z"] {
            p.key(k);
        }
        assert!(p.matches.is_empty());
        assert_eq!(p.key("enter"), PaletteEffect::Stay);
        assert_eq!(p.key("down"), PaletteEffect::Stay);
        assert_eq!(p.cursor, 0);
        assert_eq!(p.key("escape"), PaletteEffect::Close);
    }

    #[test]
    fn word_starts_rank_first() {
        let hits = search(&[Scope::Code], "nc");
        assert_eq!(hits.first().map(|a| a.id), Some("code.next_change"));
    }

    #[test]
    fn titles_holding_the_query_come_first_whatever_their_scope() {
        let map = [Scope::Map, Scope::Review, Scope::Global];
        let ids: Vec<_> = search(&map, "code").iter().map(|a| a.id).collect();
        assert_eq!(&ids[..2], ["map.drill_down", "map.code"]);
        let ids: Vec<_> = search(&map, "back to the list")
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids.first(), Some(&"review.back"));
    }
}
