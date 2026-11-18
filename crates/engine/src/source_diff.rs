//! A file's side-by-side diff, highlighted, for the code view at L4 (§5.4,
//! #93): what `Request::FileDiff` answers and `bp-bench`'s `open_50k_file`
//! measures.

use blueprint_lang_rust::Rust;
use blueprint_model::RepoPath;
use blueprint_model::diff::{DiffRow, FileDiff, FileText, LineSpan, RowKind};
use blueprint_model::lang::{Highlight, LanguageAnalyser};

/// The analyser that reads `path`, if any, for its highlighting.
fn analyser_for(path: &RepoPath) -> Option<&'static dyn LanguageAnalyser> {
    static RUST: Rust = Rust;
    RUST.handles(path).then_some(&RUST as &dyn LanguageAnalyser)
}

/// The side-by-side diff of `path` between `base` and `head` bytes (`None`
/// where the file does not exist). With `highlighted`, each side is also
/// coloured by the analyser that reads it, if there is one, both sides at
/// once. Highlighting parses the whole file, which is most of the time on a
/// large one (about 300 ms for 50,000 lines of Rust), so the service sends
/// the plain diff first.
#[must_use]
pub fn file_diff(
    path: &RepoPath,
    base: Option<&[u8]>,
    head: Option<&[u8]>,
    highlighted: bool,
) -> FileDiff {
    let analyser = analyser_for(path).filter(|_| highlighted);
    let (b, h) = (base.unwrap_or_default(), head.unwrap_or_default());
    let (base_text, (head_text, rows)) =
        rayon::join(|| text(b, analyser), || (text(h, analyser), rows(b, h)));
    FileDiff {
        base: base_text,
        head: head_text,
        rows,
    }
}

/// The lines of `bytes`, with each line's coloured tokens.
fn text(bytes: &[u8], analyser: Option<&dyn LanguageAnalyser>) -> FileText {
    let highlights = analyser.map(|a| a.highlight(bytes)).unwrap_or_default();
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut next = highlights.iter().peekable();
    let mut start = 0usize;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let end = start + line.len();
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        let mut here = Vec::new();
        // Tokens that end before this line are done; a token across lines
        // (a block comment) colours its part of each.
        while let Some(h) = next.peek() {
            if (h.end as usize) <= start {
                next.next();
            } else {
                break;
            }
        }
        for h in next.clone() {
            let (s, e) = (h.start as usize, h.end as usize);
            if s >= end {
                break;
            }
            let (s, e) = (s.max(start) - start, e.min(start + body.len()) - start);
            if e > s {
                here.push(span(s, e, h));
            }
        }
        lines.push(String::from_utf8_lossy(body).into_owned());
        spans.push(here);
        start = end;
    }
    FileText {
        lines,
        highlights: spans,
    }
}

fn span(start: usize, end: usize, h: &Highlight) -> LineSpan {
    LineSpan {
        start: u32::try_from(start).unwrap_or(u32::MAX),
        end: u32::try_from(end).unwrap_or(u32::MAX),
        class: h.class,
    }
}

/// The rows of a line diff: unchanged lines side by side, and each hunk's
/// removed and added lines paired as changed, the rest of the longer side
/// alone.
fn rows(base: &[u8], head: &[u8]) -> Vec<DiffRow> {
    use imara_diff::{Algorithm, Diff, InternedInput};
    let input = InternedInput::new(
        imara_diff::sources::byte_lines(base),
        imara_diff::sources::byte_lines(head),
    );
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let (nb, nh) = (input.before.len() as u32, input.after.len() as u32);
    let mut out = Vec::with_capacity(nb.max(nh) as usize);
    let (mut b, mut h) = (0u32, 0u32);
    let same = |out: &mut Vec<DiffRow>, b: &mut u32, h: &mut u32, until_b: u32| {
        while *b < until_b {
            out.push(DiffRow {
                base: Some(*b),
                head: Some(*h),
                kind: RowKind::Same,
            });
            *b += 1;
            *h += 1;
        }
    };
    for hunk in diff.hunks() {
        same(&mut out, &mut b, &mut h, hunk.before.start);
        let (removed, added) = (hunk.before.len() as u32, hunk.after.len() as u32);
        for i in 0..removed.max(added) {
            let (l, r) = ((i < removed).then_some(b + i), (i < added).then_some(h + i));
            out.push(DiffRow {
                base: l,
                head: r,
                kind: match (l, r) {
                    (Some(_), Some(_)) => RowKind::Changed,
                    (Some(_), None) => RowKind::Removed,
                    _ => RowKind::Added,
                },
            });
        }
        b = hunk.before.end;
        h = hunk.after.end;
    }
    same(&mut out, &mut b, &mut h, nb);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> RepoPath {
        RepoPath::parse(p).unwrap_or_else(|_| RepoPath::root())
    }

    #[test]
    fn rows_pair_changes_and_keep_the_rest_side_by_side() {
        let d = file_diff(
            &path("a.txt"),
            Some(b"one\ntwo\nthree\nfour\n"),
            Some(b"one\n2\nthree\nfour\nfive\n"),
            true,
        );
        let kinds: Vec<RowKind> = d.rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RowKind::Same,
                RowKind::Changed,
                RowKind::Same,
                RowKind::Same,
                RowKind::Added
            ]
        );
        assert_eq!(d.rows[4].head, Some(4));
        assert_eq!(d.base.lines.len(), 4);
        assert!(d.head.highlights.iter().all(Vec::is_empty), "no analyser");
    }

    #[test]
    fn rust_is_highlighted_per_line_and_missing_sides_are_empty() {
        let plain = file_diff(&path("src/lib.rs"), None, Some(b"fn f() {}\n"), false);
        assert!(plain.head.highlights.iter().all(Vec::is_empty));
        let d = file_diff(
            &path("src/lib.rs"),
            None,
            Some(b"/* a\nb */ fn f() {}\r\n"),
            true,
        );
        assert!(d.base.lines.is_empty());
        assert!(d.rows.iter().all(|r| r.kind == RowKind::Added));
        assert_eq!(d.head.lines, vec!["/* a", "b */ fn f() {}"]);
        // The comment colours its part of both lines.
        assert_eq!(d.head.highlights[0][0].end, 4);
        assert_eq!(d.head.highlights[1][0].start, 0);
        assert_eq!(d.head.highlights[1][0].end, 4);
        let line = &d.head.lines[1];
        let fname = d.head.highlights[1]
            .iter()
            .find(|s| &line[s.start as usize..s.end as usize] == "f");
        assert!(fname.is_some(), "{:?}", d.head.highlights[1]);
    }
}
