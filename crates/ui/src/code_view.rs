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
    /// For a node shown by directory: its changed files, which `]` and
    /// `[` step through (#169). Empty for a file.
    pub files: Vec<String>,
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
            files: Vec::new(),
        }
    }

    /// Takes the changed files an answer lists. Only a directory's answer
    /// lists any; the answers for its files, when stepping, list none and
    /// leave the list as it was.
    pub fn take_files(&mut self, files: Vec<String>) {
        if !files.is_empty() {
            self.files = files;
        }
    }

    /// Which of the directory's changed files is shown, counted from 1,
    /// and how many there are; `None` unless there are two or more.
    #[must_use]
    pub fn file_position(&self) -> Option<(usize, usize)> {
        if self.files.len() < 2 {
            return None;
        }
        let i = self.files.iter().position(|f| *f == self.path)?;
        Some((i + 1, self.files.len()))
    }

    /// Moves to the next (or previous) changed file of the directory, with
    /// no wrapping: the file to ask the engine for, if there is one. The
    /// view starts it afresh, waiting for its diff.
    pub fn step_file(&mut self, forward: bool) -> Option<String> {
        let (at, _) = self.file_position()?;
        let i = at - 1;
        let next = if forward { i + 1 } else { i.checked_sub(1)? };
        let path = self.files.get(next)?.clone();
        self.path.clone_from(&path);
        self.lines = None;
        self.column = 0;
        self.diff = None;
        self.detail = None;
        self.cursor = 0;
        self.opened_at = 0;
        self.error = None;
        Some(path)
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

    /// What a screen reader announces for the view (#118): the file, the
    /// node, and the row under the cursor with its lines on both sides.
    #[must_use]
    pub fn a11y_label(&self) -> String {
        let head = match self.file_position() {
            Some((k, n)) => format!("Code of {} in {}, file {k} of {n}", self.title, self.path),
            None => format!("Code of {} in {}", self.title, self.path),
        };
        if let Some(e) = &self.error {
            return format!("{head}: cannot open: {e}");
        }
        let Some(diff) = &self.diff else {
            return format!("{head}: loading");
        };
        let Some(row) = diff.rows.get(self.cursor) else {
            return format!("{head}: no rows");
        };
        fn line(
            text: &blueprint_protocol::model::diff::FileText,
            n: Option<u32>,
        ) -> Option<(u32, &str)> {
            n.and_then(|n| text.lines.get(n as usize).map(|l| (n, l.trim())))
        }
        let what = match row.kind {
            RowKind::Same => "unchanged",
            RowKind::Changed => "changed",
            RowKind::Removed => "removed",
            RowKind::Added => "added",
        };
        let sides = match (line(&diff.base, row.base), line(&diff.head, row.head)) {
            (Some((b, _)), Some((h, ht))) if row.kind == RowKind::Same => {
                format!("line {} in base, {} in head: {ht}", b + 1, h + 1)
            }
            (Some((b, bt)), Some((h, ht))) => {
                format!("base line {}: {bt} | head line {}: {ht}", b + 1, h + 1)
            }
            (Some((b, bt)), None) => format!("base line {}: {bt}", b + 1),
            (None, Some((h, ht))) => format!("head line {}: {ht}", h + 1),
            (None, None) => String::new(),
        };
        format!(
            "{head}. Row {} of {}, {what}: {sides}",
            self.cursor + 1,
            diff.rows.len()
        )
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
    use blueprint_protocol::model::diff::FileText;

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

    #[test]
    fn a_directorys_code_view_steps_through_its_changed_files() {
        let mut v = CodeView::new("docs".into(), "docs/".into(), None, false);
        // A file's view, or a directory with one file, has nowhere to go.
        assert_eq!(v.step_file(true), None);
        v.take_files(vec![
            "docs/a.md".into(),
            "docs/b.md".into(),
            "docs/c.md".into(),
        ]);
        v.path = "docs/a.md".into();
        assert_eq!(v.file_position(), Some((1, 3)));
        assert_eq!(v.step_file(false), None, "no wrapping before the first");
        assert_eq!(v.step_file(true).as_deref(), Some("docs/b.md"));
        // Stepping starts the new file afresh.
        assert_eq!(v.path, "docs/b.md");
        assert!(v.diff.is_none() && v.detail.is_none() && v.error.is_none());
        assert_eq!((v.cursor, v.column), (0, 0));
        assert_eq!(v.file_position(), Some((2, 3)));
        assert!(v.a11y_label().contains("file 2 of 3"), "{}", v.a11y_label());
        assert_eq!(v.step_file(true).as_deref(), Some("docs/c.md"));
        assert_eq!(v.step_file(true), None, "no wrapping past the last");
        assert_eq!(v.step_file(false).as_deref(), Some("docs/b.md"));
        // A file's own answer lists no files: the directory's list stays.
        v.take_files(Vec::new());
        assert_eq!(v.file_position(), Some((2, 3)));
    }

    #[test]
    fn the_screen_reader_label_names_the_file_and_reads_the_row() {
        let mut v = CodeView::new("src/a.rs".into(), "f".into(), None, false);
        assert_eq!(v.a11y_label(), "Code of f in src/a.rs: loading");
        let text = |lines: &[&str]| FileText {
            lines: lines.iter().map(|l| (*l).to_string()).collect(),
            highlights: vec![Vec::new(); lines.len()],
        };
        let diff = FileDiff {
            base: text(&["fn f() {", "    old();"]),
            head: text(&["fn f() {", "    new();"]),
            rows: vec![
                DiffRow {
                    base: Some(0),
                    head: Some(0),
                    kind: RowKind::Same,
                },
                DiffRow {
                    base: Some(1),
                    head: Some(1),
                    kind: RowKind::Changed,
                },
            ],
        };
        v.receive(Detail::Full, Arc::new(diff));
        v.cursor = 0;
        assert_eq!(
            v.a11y_label(),
            "Code of f in src/a.rs. Row 1 of 2, unchanged: line 1 in base, 1 in head: fn f() {"
        );
        v.cursor = 1;
        assert_eq!(
            v.a11y_label(),
            "Code of f in src/a.rs. Row 2 of 2, changed: base line 2: old(); | head line 2: new();"
        );
    }
}
