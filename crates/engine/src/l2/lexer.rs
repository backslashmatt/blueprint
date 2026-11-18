//! A shallow Rust reader for module declarations.
//!
//! L2 only needs each file's `mod` items: their names, visibility, the
//! `#[path]` and `#[cfg_attr(.., path = ..)]` attributes written before them,
//! and the bodies of inline modules. A full syntax tree costs far more than
//! that; on rust-analyzer it took about 2 s, against a 1 s budget for the
//! whole L1–L2 model. So this lexes the source once (skipping comments,
//! strings, raw strings and character literals correctly), matches brackets,
//! and reads items from the tokens. Item bodies other than inline modules
//! and macro calls are skipped by their bracket match.
//!
//! A file with an unterminated comment or string, or unbalanced brackets, is
//! reported as not clean, so its module is degraded to `unknown` (§5.5); the
//! items before the damage are still read.

use blueprint_model::Visibility;

/// Inline modules nested deeper than this are not read; the walker stops at
/// the same depth.
const MAX_INLINE_DEPTH: usize = 65;

#[derive(Debug, Clone, PartialEq)]
enum Tok<'s> {
    Ident(&'s str),
    Str(String),
    Punct(char),
    Open(char),
    Close(char),
}

/// A `mod` declaration found in a file.
#[derive(Debug, Clone, PartialEq)]
pub struct ModItem {
    /// The module's name (a raw identifier's `r#` removed).
    pub name: String,
    /// The items of an inline module (`mod x { .. }`), or `None` for `mod x;`.
    pub body: Option<Vec<ModItem>>,
    /// The value of a `#[path = ".."]` attribute.
    pub path_attr: Option<String>,
    /// Whether a `#[cfg_attr(.., path = ..)]` may change the file.
    pub cfg_attr_path: bool,
    /// Whether it was found inside a macro call (as with `cfg_if!`).
    pub in_macro: bool,
    /// `pub`, `pub(..)` or private.
    pub visibility: Visibility,
}

/// What a file declares.
#[derive(Debug, Default, PartialEq)]
pub struct Scan {
    /// Its `mod` items, in order.
    pub items: Vec<ModItem>,
    /// False if a comment or string is unterminated or brackets do not match.
    pub clean: bool,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Tokenises `src`. The second value is false if a comment or string runs to
/// the end of the file.
fn lex(src: &str) -> (Vec<Tok<'_>>, bool) {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut clean = true;
    // The byte at `j`, or 0 past the end.
    let at = |j: usize| -> u8 { if j < n { b[j] } else { 0 } };
    // Skips a (possibly raw) string starting at the opening quote `q`, with
    // `hashes` `#`s for raw strings. Returns (content, index after it).
    let string = |q: usize, raw: bool, hashes: usize| -> (String, usize, bool) {
        let mut j = q + 1;
        let mut s = String::new();
        let start = j;
        while j < n {
            match b[j] {
                b'\\' if !raw => {
                    let hex = |from: usize, to: usize| -> Option<char> {
                        u32::from_str_radix(src.get(from..to)?, 16)
                            .ok()
                            .and_then(char::from_u32)
                    };
                    match at(j + 1) {
                        e @ (b'\\' | b'"' | b'\'') => {
                            s.push(e as char);
                            j += 2;
                        }
                        b'n' => {
                            s.push('\n');
                            j += 2;
                        }
                        b't' => {
                            s.push('\t');
                            j += 2;
                        }
                        b'r' => {
                            s.push('\r');
                            j += 2;
                        }
                        b'0' => {
                            s.push('\0');
                            j += 2;
                        }
                        b'x' => {
                            s.extend(hex(j + 2, j + 4));
                            j += 4;
                        }
                        b'u' if at(j + 2) == b'{' => {
                            let close = (j + 3..n.min(j + 12))
                                .find(|&k| b[k] == b'}')
                                .unwrap_or(j + 2);
                            s.extend(hex(j + 3, close));
                            j = close + 1;
                        }
                        b'\n' | b'\r' => {
                            // A line continuation: skip the newline and the
                            // whitespace that follows.
                            j += 1;
                            while j < n && matches!(b[j], b' ' | b'\t' | b'\n' | b'\r') {
                                j += 1;
                            }
                        }
                        _ => j += 2,
                    }
                    continue;
                }
                b'"' if (0..hashes).all(|h| at(j + 1 + h) == b'#') => {
                    if raw {
                        s = src.get(start..j).unwrap_or("").to_owned();
                    }
                    return (s, j + 1 + hashes, true);
                }
                _ => {}
            }
            if !raw {
                // Copy one UTF-8 character.
                let ch_len = src
                    .get(j..)
                    .and_then(|r| r.chars().next())
                    .map_or(1, char::len_utf8);
                s.push_str(src.get(j..j + ch_len).unwrap_or(""));
                j += ch_len;
            } else {
                j += 1;
            }
        }
        (s, n, false)
    };
    while i < n {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'/' if at(i + 1) == b'/' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if at(i + 1) == b'*' => {
                let mut depth = 0usize;
                loop {
                    if i >= n {
                        clean = false;
                        break;
                    }
                    if b[i] == b'/' && at(i + 1) == b'*' {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && at(i + 1) == b'/' {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => {
                let (s, next, ok) = string(i, false, 0);
                clean &= ok;
                out.push(Tok::Str(s));
                i = next;
            }
            b'\'' => {
                // A character literal ('x', '\n', '\u{..}') or a lifetime ('a).
                if at(i + 1) == b'\\' {
                    // Skip the escaped character itself (it may be `'`).
                    let mut j = i + 3;
                    while j < n && b[j] != b'\'' && b[j] != b'\n' {
                        j += 1;
                    }
                    i = j + 1;
                } else {
                    let ch_len = src
                        .get(i + 1..)
                        .and_then(|r| r.chars().next())
                        .map_or(1, char::len_utf8);
                    if at(i + 1 + ch_len) == b'\'' {
                        i += 2 + ch_len;
                    } else {
                        i += 1; // a lifetime: its name lexes as an identifier
                    }
                }
            }
            b'(' | b'[' | b'{' => {
                out.push(Tok::Open(c as char));
                i += 1;
            }
            b')' | b']' | b'}' => {
                out.push(Tok::Close(c as char));
                i += 1;
            }
            _ => {
                let ch = src.get(i..).and_then(|r| r.chars().next()).unwrap_or('\0');
                if is_ident_start(ch) {
                    let start = i;
                    while i < n {
                        let d = src.get(i..).and_then(|r| r.chars().next()).unwrap_or('\0');
                        if !is_ident(d) {
                            break;
                        }
                        i += d.len_utf8();
                    }
                    let word = src.get(start..i).unwrap_or("");
                    // String prefixes: b"..", c"..", r"..", r#".."#, br"..", cr"..".
                    let raw = matches!(word, "r" | "br" | "cr");
                    if matches!(word, "b" | "c") && at(i) == b'"' {
                        let (s, next, ok) = string(i, false, 0);
                        clean &= ok;
                        out.push(Tok::Str(s));
                        i = next;
                    } else if raw && (at(i) == b'"' || at(i) == b'#') {
                        let mut h = 0;
                        while at(i + h) == b'#' {
                            h += 1;
                        }
                        if at(i + h) == b'"' {
                            let (s, next, ok) = string(i + h, true, h);
                            clean &= ok;
                            out.push(Tok::Str(s));
                            i = next;
                        } else if word == "r"
                            && h == 1
                            && src
                                .get(i + 1..)
                                .and_then(|r| r.chars().next())
                                .is_some_and(is_ident_start)
                        {
                            // A raw identifier, r#name.
                            let start = i + 1;
                            i = start;
                            while i < n {
                                let d = src.get(i..).and_then(|r| r.chars().next()).unwrap_or('\0');
                                if !is_ident(d) {
                                    break;
                                }
                                i += d.len_utf8();
                            }
                            // Kept with its `r#`, so `r#mod` is never the keyword.
                            out.push(Tok::Ident(src.get(start - 2..i).unwrap_or("")));
                        } else {
                            out.push(Tok::Ident(word));
                        }
                    } else {
                        out.push(Tok::Ident(word));
                    }
                } else if ch.is_ascii_digit() {
                    while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.') {
                        // Stop before `..` so ranges keep their punctuation.
                        if b[i] == b'.' && at(i + 1) == b'.' {
                            break;
                        }
                        i += 1;
                    }
                } else {
                    out.push(Tok::Punct(ch));
                    i += ch.len_utf8().max(1);
                }
            }
        }
    }
    (out, clean)
}

/// For each opening token, the index of its closing token (or the end, if
/// unclosed). The second value is false if brackets do not match.
fn matching(toks: &[Tok<'_>]) -> (Vec<usize>, bool) {
    let mut close = vec![toks.len(); toks.len()];
    let mut stack: Vec<(usize, char)> = Vec::new();
    let mut ok = true;
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::Open(c) => stack.push((i, *c)),
            Tok::Close(c) => {
                let want = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                match stack.pop() {
                    Some((o, oc)) if oc == want => close[o] = i,
                    Some((o, _)) => {
                        ok = false;
                        close[o] = i;
                    }
                    None => ok = false,
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        ok = false;
    }
    (close, ok)
}

struct Reader<'t, 's> {
    toks: &'t [Tok<'s>],
    close: &'t [usize],
}

impl Reader<'_, '_> {
    fn tok(&self, i: usize) -> Option<&Tok<'_>> {
        self.toks.get(i)
    }

    /// The items between `start` and `end` (exclusive), at nesting depth
    /// `depth`. In a macro call (`in_macro`), `{..}` groups that follow `]`,
    /// `else` or `=>` (as in `cfg_if!`) are read as item lists too; other
    /// bodies, such as function bodies, are still skipped.
    fn items(&self, start: usize, end: usize, depth: usize, in_macro: bool) -> Vec<ModItem> {
        let mut out = Vec::new();
        let (mut path_attr, mut cfg_attr_path, mut vis) = (None, false, Visibility::Private);
        let mut i = start;
        while i < end {
            match self.tok(i) {
                Some(Tok::Punct('#')) => {
                    let inner = self.tok(i + 1) == Some(&Tok::Punct('!'));
                    let open = if inner { i + 2 } else { i + 1 };
                    if self.tok(open) == Some(&Tok::Open('[')) {
                        let close = self.close[open].min(end);
                        if !inner {
                            self.attribute(open + 1, close, &mut path_attr, &mut cfg_attr_path);
                        }
                        i = close + 1;
                    } else {
                        i += 1;
                    }
                }
                Some(Tok::Ident("pub")) => {
                    if self.tok(i + 1) == Some(&Tok::Open('(')) {
                        vis = Visibility::Restricted;
                        i = self.close[i + 1].min(end) + 1;
                    } else {
                        vis = Visibility::Public;
                        i += 1;
                    }
                }
                Some(Tok::Ident("crate")) if self.tok(i + 1) == Some(&Tok::Ident("mod")) => {
                    vis = Visibility::Restricted;
                    i += 1;
                }
                Some(Tok::Ident("mod")) => {
                    let Some(Tok::Ident(name)) = self.tok(i + 1) else {
                        i += 1;
                        continue;
                    };
                    let name = name.strip_prefix("r#").unwrap_or(name).to_string();
                    match self.tok(i + 2) {
                        Some(Tok::Punct(';')) => {
                            out.push(ModItem {
                                name,
                                body: None,
                                path_attr: path_attr.take(),
                                cfg_attr_path,
                                in_macro,
                                visibility: vis,
                            });
                            i += 3;
                        }
                        Some(Tok::Open('{')) => {
                            let close = self.close[i + 2].min(end);
                            let body = if depth < MAX_INLINE_DEPTH {
                                self.items(i + 3, close, depth + 1, false)
                            } else {
                                Vec::new()
                            };
                            out.push(ModItem {
                                name,
                                body: Some(body),
                                path_attr: path_attr.take(),
                                cfg_attr_path,
                                in_macro,
                                visibility: vis,
                            });
                            i = close + 1;
                        }
                        _ => i += 2,
                    }
                    (path_attr, cfg_attr_path, vis) = (None, false, Visibility::Private);
                }
                Some(Tok::Ident(_))
                    if self.tok(i + 1) == Some(&Tok::Punct('!'))
                        && matches!(self.tok(i + 2), Some(Tok::Open(_))) =>
                {
                    // A macro call such as cfg_if! { .. }: read the items inside.
                    let close = self.close[i + 2].min(end);
                    if depth < MAX_INLINE_DEPTH {
                        out.extend(self.items(i + 3, close, depth + 1, true));
                    }
                    i = close + 1;
                    (path_attr, cfg_attr_path, vis) = (None, false, Visibility::Private);
                }
                Some(Tok::Open('{'))
                    if in_macro
                        && (i == start
                            || matches!(
                                self.tok(i - 1),
                                Some(Tok::Close(']') | Tok::Ident("else") | Tok::Punct('>'))
                            )) =>
                {
                    // A branch of a macro such as cfg_if!: an item list.
                    let close = self.close[i].min(end);
                    if depth < MAX_INLINE_DEPTH {
                        out.extend(self.items(i + 1, close, depth + 1, true));
                    }
                    i = close + 1;
                    (path_attr, cfg_attr_path, vis) = (None, false, Visibility::Private);
                }
                Some(Tok::Open(_)) => {
                    // Any other body or group: skip it whole.
                    i = self.close[i].min(end) + 1;
                    (path_attr, cfg_attr_path, vis) = (None, false, Visibility::Private);
                }
                Some(Tok::Punct(';')) => {
                    i += 1;
                    (path_attr, cfg_attr_path, vis) = (None, false, Visibility::Private);
                }
                _ => i += 1,
            }
        }
        out
    }

    /// Reads `path = ".."` or `cfg_attr(.., path = ..)` from an attribute's tokens.
    fn attribute(&self, start: usize, end: usize, path: &mut Option<String>, cfg_path: &mut bool) {
        match self.tok(start) {
            Some(Tok::Ident("path")) => {
                if let (Some(Tok::Punct('=')), Some(Tok::Str(v))) =
                    (self.tok(start + 1), self.tok(start + 2))
                {
                    *path = Some(v.clone());
                }
            }
            Some(Tok::Ident("cfg_attr")) => {
                let sets = (start..end).any(|k| {
                    self.tok(k) == Some(&Tok::Ident("path"))
                        && self.tok(k + 1) == Some(&Tok::Punct('='))
                });
                *cfg_path |= sets;
            }
            _ => {}
        }
    }
}

/// Reads the `mod` items of one Rust source file.
pub fn scan(src: &str) -> Scan {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let (toks, lex_ok) = lex(src);
    let (close, brackets_ok) = matching(&toks);
    let reader = Reader {
        toks: &toks,
        close: &close,
    };
    Scan {
        items: reader.items(0, toks.len(), 0, false),
        clean: lex_ok && brackets_ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[ModItem]) -> Vec<String> {
        let mut out = Vec::new();
        for i in items {
            out.push(i.name.clone());
            if let Some(b) = &i.body {
                out.extend(names(b).into_iter().map(|n| format!("{}::{n}", i.name)));
            }
        }
        out
    }

    #[test]
    fn reads_mod_items_past_comments_strings_and_bodies() {
        let src = r##"
//! mod not_a_module;
/* mod also_not; /* nested */ */
const S: &str = "mod in_string;";
const R: &str = r#"mod in_raw "string";"#;
const C: char = '{';
fn f<'a>(x: &'a str) { mod in_fn; let y = '"'; }
#[path = "other.rs"]
pub mod a;
#[cfg_attr(test, path = "t.rs")]
pub(crate) mod b;
mod r#type;
pub mod inline {
    #[cfg(test)]
    mod deep;
}
cfg_if::cfg_if! { if #[cfg(unix)] { pub mod unix; } else { pub(crate) mod other; } }
impl X { fn g() {} }
"##;
        let s = scan(src);
        assert!(s.clean);
        assert_eq!(
            names(&s.items),
            ["a", "b", "type", "inline", "inline::deep", "unix", "other"]
        );
        assert_eq!(s.items[0].path_attr.as_deref(), Some("other.rs"));
        assert_eq!(s.items[0].visibility, Visibility::Public);
        assert!(s.items[1].cfg_attr_path);
        assert_eq!(s.items[1].visibility, Visibility::Restricted);
        assert_eq!(s.items[2].visibility, Visibility::Private);
        // items[3] is `inline`, whose child `deep` is in its body.
        assert!(s.items[4].in_macro);
        assert_eq!(s.items[4].visibility, Visibility::Public);
        assert_eq!(s.items[5].visibility, Visibility::Restricted);
    }

    #[test]
    fn damage_is_reported_and_earlier_items_kept() {
        for src in [
            "mod a; fn f( {",
            "mod a; /* open",
            "mod a; const S: &str = \"open",
            "mod a; ]]]",
        ] {
            let s = scan(src);
            assert!(!s.clean, "{src:?}");
            assert_eq!(names(&s.items), ["a"], "{src:?}");
        }
    }

    #[test]
    fn deep_nesting_and_hostile_input_finish() {
        let deep = format!("{}{}", "mod a{".repeat(5_000), "}".repeat(5_000));
        let s = scan(&deep);
        assert!(s.clean);
        let brackets = "{".repeat(1_000_000);
        assert!(!scan(&brackets).clean);
        let weird = "'\\u{1F600}' 'é' b'x' br##\"x\"## \u{0}\u{1}";
        let _ = scan(weird);
    }

    #[test]
    fn review_findings_escapes_macros_raw_identifiers_and_cost() {
        // An escaped quote in a char literal does not desynchronise.
        let s = scan("const Q: [char; 2] = ['\\'','\"'];\nmod hidden;\nconst S: &str = \"x\";");
        assert!(s.clean);
        assert_eq!(names(&s.items), ["hidden"]);

        // #[path] escapes decode as rustc does.
        for (src, want) in [
            ("#[path = \"a\\x2ers\"] mod a;", "a.rs"),
            ("#[path = \"c\\u{2e}rs\"] mod c;", "c.rs"),
            ("#[path = \"dir\\\n    /b.rs\"] mod b;", "dir/b.rs"),
        ] {
            assert_eq!(
                scan(src).items[0].path_attr.as_deref(),
                Some(want),
                "{src:?}"
            );
        }

        // Inside cfg_if!, attributes and nesting are kept and fn bodies skipped.
        let s = scan(
            "cfg_if! { if #[cfg(unix)] { #[path=\"sys/unix.rs\"] mod imp; mod sys { pub mod inner; } fn f() { mod local; } } else { pub(crate) mod other; } }",
        );
        assert_eq!(names(&s.items), ["imp", "sys", "sys::inner", "other"]);
        assert_eq!(s.items[0].path_attr.as_deref(), Some("sys/unix.rs"));
        assert!(s.items.iter().all(|i| i.in_macro));
        assert_eq!(s.items[2].visibility, Visibility::Restricted);

        // r#mod is an identifier, not the keyword; crate mod is restricted.
        assert!(names(&scan("impl r#mod r#x;").items).is_empty());
        assert_eq!(
            scan("crate mod c;").items[0].visibility,
            Visibility::Restricted
        );

        // pub(..) mod inside deeply nested macro groups stays linear.
        let k = 20_000;
        let src = format!(
            "m! {{ {}{} }}",
            "(".repeat(k),
            ") pub(crate) mod a;".repeat(k)
        );
        let t = std::time::Instant::now();
        let _ = scan(&src);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            t.elapsed()
        );
    }
}
