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
#[derive(Debug, Default)]
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
                    if j + 1 < n {
                        match b[j + 1] {
                            b'\\' => s.push('\\'),
                            b'"' => s.push('"'),
                            b'\'' => s.push('\''),
                            b'n' => s.push('\n'),
                            b't' => s.push('\t'),
                            _ => {}
                        }
                    }
                    j += 2;
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
                    let mut j = i + 2;
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
                            out.push(Tok::Ident(src.get(start..i).unwrap_or("")));
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

    /// The items between `start` and `end` (exclusive), at inline depth `depth`.
    fn items(&self, start: usize, end: usize, depth: usize) -> Vec<ModItem> {
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
                Some(Tok::Ident("mod")) => {
                    let Some(Tok::Ident(name)) = self.tok(i + 1) else {
                        i += 1;
                        continue;
                    };
                    let name = name.to_string();
                    match self.tok(i + 2) {
                        Some(Tok::Punct(';')) => {
                            out.push(ModItem {
                                name,
                                body: None,
                                path_attr: path_attr.take(),
                                cfg_attr_path,
                                in_macro: false,
                                visibility: vis,
                            });
                            i += 3;
                        }
                        Some(Tok::Open('{')) => {
                            let close = self.close[i + 2].min(end);
                            let body = if depth < MAX_INLINE_DEPTH {
                                self.items(i + 3, close, depth + 1)
                            } else {
                                Vec::new()
                            };
                            out.push(ModItem {
                                name,
                                body: Some(body),
                                path_attr: path_attr.take(),
                                cfg_attr_path,
                                in_macro: false,
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
                    // A macro call such as cfg_if! { .. }: look for `[pub] mod x;`.
                    let close = self.close[i + 2].min(end);
                    out.extend(self.macro_mods(i + 3, close));
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

    /// `[pub[(..)]] mod name;` sequences anywhere inside a macro call.
    fn macro_mods(&self, start: usize, end: usize) -> Vec<ModItem> {
        let mut out = Vec::new();
        for k in start..end {
            if let (Some(Tok::Ident("mod")), Some(Tok::Ident(name)), Some(Tok::Punct(';'))) =
                (self.tok(k), self.tok(k + 1), self.tok(k + 2))
            {
                let visibility =
                    match (k.checked_sub(1).and_then(|p| self.tok(p)), k.checked_sub(1)) {
                        (Some(Tok::Ident("pub")), _) => Visibility::Public,
                        (Some(Tok::Close(')')), Some(p)) => {
                            // `pub(crate) mod x;`: find the `(` and check for `pub` before it.
                            let open = (start..p).rev().find(|&o| self.close.get(o) == Some(&p));
                            match open
                                .and_then(|o| o.checked_sub(1))
                                .and_then(|o| self.tok(o))
                            {
                                Some(Tok::Ident("pub")) => Visibility::Restricted,
                                _ => Visibility::Private,
                            }
                        }
                        _ => Visibility::Private,
                    };
                out.push(ModItem {
                    name: name.to_string(),
                    body: None,
                    path_attr: None,
                    cfg_attr_path: false,
                    in_macro: true,
                    visibility,
                });
            }
        }
        out
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
        items: reader.items(0, toks.len(), 0),
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
}
