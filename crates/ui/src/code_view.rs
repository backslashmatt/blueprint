//! The code view (REQUIREMENTS §5.4, view 4 at L4; #93): one file's
//! side-by-side diff, opened from a node on the map and scrolled to the
//! node's lines. View state only, so it is tested headlessly; `workspace`
//! draws the rows on screen and nothing else.

use std::sync::Arc;

use blueprint_protocol::Detail;
use blueprint_protocol::model::diff::{DiffRow, FileDiff, RowKind};

/// Rows a page key moves until the window has been measured.
pub const PAGE: usize = 30;

/// Rows a page key moves in a list `height` pixels tall whose rows are
/// `row` pixels, both as last laid out: a screenful less a row, so no row
/// is passed over unseen. `None` until the list has been laid out.
#[must_use]
pub fn page_rows(height: f32, row: f32) -> Option<usize> {
    if !(row > 0.0 && height > 0.0 && height.is_finite()) {
        return None;
    }
    let rows = (height / row).floor();
    Some(if rows < 2.0 { 1 } else { rows as usize - 1 })
}

/// Characters a left or right key scrolls.
pub const COLUMNS: usize = 8;

/// One file's diff on screen.
#[derive(Debug, Clone)]
pub struct CodeView {
    /// The file, `/`-separated.
    pub path: String,
    /// What was opened, for the header: the node's name.
    pub title: String,
    /// The node's lines, 1-based, to scroll to.
    pub lines: Option<(u32, u32)>,
    /// Whether `lines` are base's (a node only in base) rather than head's.
    pub lines_in_base: bool,
    /// Characters scrolled off the left of both sides, for lines wider
    /// than their half of the window.
    pub column: usize,
    /// The diff once it has arrived: plain first, then highlighted.
    pub diff: Option<Arc<FileDiff>>,
    /// How much of it has arrived.
    pub detail: Option<Detail>,
    /// The row under the cursor.
    pub cursor: usize,
    /// The row the cursor was put on when the diff first arrived.
    pub opened_at: usize,
    /// Rows a page key moves: set from the window on every frame.
    pub page: usize,
    /// Why the diff could not be had, if it could not.
    pub error: Option<String>,
}

impl CodeView {
    /// A view of `path`, waiting for its diff.
    #[must_use]
    pub fn new(
        path: String,
        title: String,
        lines: Option<(u32, u32)>,
        lines_in_base: bool,
    ) -> CodeView {
        CodeView {
            path,
            title,
            lines,
            lines_in_base,
            column: 0,
            diff: None,
            detail: None,
            cursor: 0,
            opened_at: 0,
            page: PAGE,
            error: None,
        }
    }

    /// Takes in a diff. The first one puts the cursor on the node's first
    /// line (or the first change); a later one, the highlighted version of
    /// the same diff, keeps it where it is.
    pub fn receive(&mut self, detail: Detail, diff: Arc<FileDiff>) {
        let first = self.diff.is_none();
        if first {
            self.cursor = focus(&diff.rows, self.lines, self.lines_in_base);
            self.opened_at = self.cursor;
        }
        self.diff = Some(diff);
        self.detail = Some(detail);
    }

    /// The number of rows.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.diff.as_ref().map_or(0, |d| d.rows.len())
    }

    /// Moves the cursor for `key` (a GPUI key name). Returns whether it
    /// moved.
    pub fn key(&mut self, key: &str) -> bool {
        let last = self.rows().saturating_sub(1);
        let to = match key {
            "up" => self.cursor.saturating_sub(1),
            "down" => (self.cursor + 1).min(last),
            "pageup" => self.cursor.saturating_sub(self.page),
            "pagedown" => (self.cursor + self.page).min(last),
            "home" => 0,
            "end" => last,
            "left" | "right" => {
                let before = self.column;
                self.column = if key == "left" {
                    self.column.saturating_sub(COLUMNS)
                } else {
                    self.column + COLUMNS
                };
                return self.column != before;
            }
            "n" => self.next_change(true).unwrap_or(self.cursor),
            "p" => self.next_change(false).unwrap_or(self.cursor),
            _ => return false,
        };
        let moved = to != self.cursor;
        self.cursor = to;
        moved
    }

    /// The first row of the next (or previous) run of changed rows.
    fn next_change(&self, forward: bool) -> Option<usize> {
        let rows = &self.diff.as_ref()?.rows;
        let changed = |i: usize| rows.get(i).is_some_and(|r| r.kind != RowKind::Same);
        let starts = |i: usize| changed(i) && (i == 0 || !changed(i - 1));
        if forward {
            (self.cursor + 1..rows.len()).find(|&i| starts(i))
        } else {
            (0..self.cursor).rev().find(|&i| starts(i))
        }
    }
}

/// The row to open on: the first row of the node's lines, on the side
/// they count (base for a node only there), else the first changed row,
/// else the top.
fn focus(rows: &[DiffRow], lines: Option<(u32, u32)>, in_base: bool) -> usize {
    if let Some((first, _)) = lines {
        let line = first.saturating_sub(1);
        let side = |r: &DiffRow| if in_base { r.base } else { r.head };
        if let Some(i) = rows.iter().position(|r| side(r).is_some_and(|l| l >= line)) {
            return i;
        }
    }
    rows.iter()
        .position(|r| r.kind != RowKind::Same)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(base: Option<u32>, head: Option<u32>, kind: RowKind) -> DiffRow {
        DiffRow { base, head, kind }
    }

    fn diff() -> Arc<FileDiff> {
        let rows = vec![
            row(Some(0), Some(0), RowKind::Same),
            row(Some(1), Some(1), RowKind::Changed),
            row(Some(2), Some(2), RowKind::Same),
            row(None, Some(3), RowKind::Added),
            row(None, Some(4), RowKind::Added),
            row(Some(3), Some(5), RowKind::Same),
            row(Some(4), None, RowKind::Removed),
        ];
        Arc::new(FileDiff {
            rows,
            ..FileDiff::default()
        })
    }

    #[test]
    fn opens_on_the_nodes_lines_and_keeps_its_place_when_highlights_arrive() {
        let mut v = CodeView::new("a.rs".into(), "f".into(), Some((5, 6)), false);
        v.receive(Detail::Coarse, diff());
        assert_eq!(v.cursor, 4, "head line 5 is row 4");
        v.key("up");
        v.receive(Detail::Full, diff());
        assert_eq!(v.cursor, 3);
        assert_eq!(v.detail, Some(Detail::Full));
        let mut none = CodeView::new("a.rs".into(), "a".into(), None, false);
        none.receive(Detail::Coarse, diff());
        assert_eq!(none.cursor, 1, "without lines, the first change");
        // A removed node's lines count base's: base line 5 is the removed
        // row 6, not head line 5 (row 4).
        let mut removed = CodeView::new("a.rs".into(), "g".into(), Some((5, 5)), true);
        removed.receive(Detail::Coarse, diff());
        assert_eq!(removed.cursor, 6);
        let mut none = CodeView::new("a.rs".into(), "a".into(), None, false);
        none.receive(Detail::Coarse, diff());
        assert_eq!(none.cursor, 1, "without lines, the first change");
    }

    #[test]
    fn keys_move_within_the_rows_and_jump_between_changes() {
        let mut v = CodeView::new("a.rs".into(), "a".into(), None, false);
        assert!(!v.key("down"), "nothing to move through yet");
        assert!(!v.key("left"), "already at the first column");
        assert!(v.key("right"));
        assert_eq!(v.column, COLUMNS);
        assert!(v.key("left"));
        assert_eq!(v.column, 0);
        v.receive(Detail::Coarse, diff());
        assert!(v.key("n"));
        assert_eq!(v.cursor, 3);
        assert!(v.key("n"));
        assert_eq!(v.cursor, 6);
        assert!(!v.key("n"), "no change after the last");
        assert!(v.key("p"));
        assert_eq!(v.cursor, 3);
        v.key("end");
        assert_eq!(v.cursor, 6);
        v.key("pagedown");
        assert_eq!(v.cursor, 6);
        v.key("pageup");
        assert_eq!(v.cursor, 0);
        assert!(!v.key("x"));
    }

    #[test]
    fn a_page_is_a_screenful_less_a_row() {
        // 640 px of 18 px rows shows 35 whole rows.
        assert_eq!(page_rows(640.0, 18.0), Some(34));
        // The same list at twice the text size.
        assert_eq!(page_rows(640.0, 36.0), Some(16));
        // A list too short for two rows still moves.
        assert_eq!(page_rows(30.0, 36.0), Some(1));
        // Not laid out yet.
        assert_eq!(page_rows(0.0, 18.0), None);
        assert_eq!(page_rows(640.0, 0.0), None);
        assert_eq!(page_rows(f32::NAN, 18.0), None);
    }
}
