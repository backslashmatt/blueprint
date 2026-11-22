//! L2 modules of a .NET project (#146, DR 0013): one module per namespace
//! its C# files declare, nested by path (`App.Core` is `["App", "Core"]`).
//! Which namespaces a file declares is found by a lexical scan, comments
//! and strings skipped, so the L1–L2 pass does not pay for a parse.
//!
//! Every C# file of a project is placed at the project's root module; each
//! item's namespace ([`blueprint_model::lang::Item::module`]) places it
//! under its namespace's module. Files under `bin/` and `obj/` are build
//! output and are shown by directory.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, Level, Model, Node, NodeId, NodeKind, RepoPath, SystemKey, Visibility,
};

use crate::l1::{Diagnostic, Read};

/// Namespaces nested deeper than this are not read.
const MAX_DEPTH: usize = 64;

/// Whether a file of the project at `key` is one of its C# sources.
fn is_source(key: &SystemKey, file: &RepoPath) -> bool {
    let rel = &file.segments()[key.dir.segments().len()..];
    rel.last().is_some_and(|f| f.ends_with(".cs"))
        && !rel[..rel.len().saturating_sub(1)]
            .iter()
            .any(|d| d == "bin" || d == "obj")
}

/// The modules of the project at `key`: a node per namespace (and each
/// enclosing one) its files declare, and every C# file placed at the root.
/// `None` if it has no C# file.
pub fn analyse_project(
    system: &NodeId,
    key: &SystemKey,
    files: &BTreeSet<&RepoPath>,
    read: Read<'_>,
    model: &mut Model,
    diags: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<RepoPath, Vec<String>>> {
    let mut reached = BTreeMap::new();
    // Each namespace, with the first file (by path) that declares it.
    let mut namespaces: BTreeMap<Vec<String>, &RepoPath> = BTreeMap::new();
    for file in files.iter().copied().filter(|f| is_source(key, f)) {
        if let Some(bytes) = read(file) {
            for ns in namespaces_of(&bytes) {
                for n in 1..=ns.len() {
                    namespaces.entry(ns[..n].to_vec()).or_insert(file);
                }
            }
        }
        reached.insert(file.clone(), Vec::new());
    }
    for (ns, file) in namespaces {
        let node = Node {
            id: NodeId::within(Level::Module, key.clone(), ns.clone()),
            kind: NodeKind::Module,
            name: ns.join("."),
            source: file.clone(),
            parent: Some(system.clone()),
            confidence: Confidence::Exact,
            visibility: Some(Visibility::Public),
            code: None,
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: file.clone(),
                message: e.to_string(),
            });
        }
    }
    (!reached.is_empty()).then_some(reached)
}

/// The namespaces a C# file declares, block or file-scoped, as nested
/// paths: `namespace A { namespace B.C {} }` gives `A` and `A.B.C`. Linear
/// in the file's length; paths deeper than [`MAX_DEPTH`] are not read.
#[must_use]
pub fn namespaces_of(bytes: &[u8]) -> BTreeSet<Vec<String>> {
    let src = String::from_utf8_lossy(bytes);
    let tokens = tokens(&src);
    let mut out = BTreeSet::new();
    // The enclosing namespaces: (path, brace depth at which it closes).
    let mut stack: Vec<(Vec<String>, usize)> = Vec::new();
    let mut file_scoped: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "{" => depth += 1,
            "}" => {
                depth = depth.saturating_sub(1);
                while stack.last().is_some_and(|(_, d)| *d > depth) {
                    stack.pop();
                }
            }
            // `@namespace` is an identifier, kept with its `@` by `tokens`.
            "namespace" => {
                // A dotted name, then `{` (block) or `;` (file-scoped).
                let mut name: Vec<&str> = Vec::new();
                let mut j = i + 1;
                while let Some(t) = tokens.get(j) {
                    if !is_ident(t) || name.len() > MAX_DEPTH {
                        break;
                    }
                    name.push(t);
                    j += 1;
                    if tokens.get(j) != Some(&".") {
                        break;
                    }
                    j += 1;
                }
                let outer_len = stack.last().map_or(file_scoped.len(), |(p, _)| p.len());
                let fits = !name.is_empty() && outer_len + name.len() <= MAX_DEPTH;
                let segs = || name.iter().map(|t| t.trim_start_matches('@').to_owned());
                match tokens.get(j) {
                    Some(&"{") if fits => {
                        let outer = stack.last().map_or(&file_scoped, |(p, _)| p);
                        let full: Vec<String> = outer.iter().cloned().chain(segs()).collect();
                        depth += 1;
                        out.insert(full.clone());
                        stack.push((full, depth));
                        i = j;
                    }
                    Some(&";") if fits && stack.is_empty() => {
                        file_scoped = segs().collect();
                        out.insert(file_scoped.clone());
                        i = j;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn is_ident(t: &str) -> bool {
    t.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '@')
}

/// Identifiers (a verbatim `@name` kept whole) and the punctuation
/// `{ } ; .`, with comments, strings, characters and preprocessor lines
/// skipped. Of `#if … #elif … #else … #endif`, only the first branch is
/// read, so both branches of a conditional `namespace X {` are not counted.
fn tokens(src: &str) -> Vec<&str> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    // For each open `#if`: whether its current branch is skipped.
    let mut pp: Vec<bool> = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        if c == b'#' && line_start {
            let end = b[i..]
                .iter()
                .position(|&x| x == b'\n')
                .map_or(b.len(), |p| i + p);
            let directive = src.get(i + 1..end).unwrap_or("").trim_start();
            let word: String = directive
                .chars()
                .take_while(|ch| ch.is_ascii_alphabetic())
                .collect();
            match word.as_str() {
                "if" => pp.push(false),
                "elif" | "else" => {
                    if let Some(top) = pp.last_mut() {
                        *top = true;
                    }
                }
                "endif" => {
                    pp.pop();
                }
                _ => {}
            }
            i = end;
            continue;
        }
        line_start = false;
        let skipping = pp.iter().any(|s| *s);
        match c {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            b'"' | b'\'' => i = skip_literal(b, i, 0),
            // A string's `$`/`@` prefix: the literal starts after it.
            b'$' | b'@' if matches!(b.get(i + 1), Some(b'"' | b'$' | b'@')) => {
                let mut j = i;
                while matches!(b.get(j), Some(b'$' | b'@')) {
                    j += 1;
                }
                i = if b.get(j) == Some(&b'"') {
                    skip_literal(b, j, 0)
                } else {
                    j
                };
            }
            b'{' | b'}' | b';' | b'.' => {
                if !skipping {
                    out.push(&src[i..=i]);
                }
                i += 1;
            }
            _ if c.is_ascii_alphabetic() || c == b'_' || c == b'@' || c >= 0x80 => {
                let start = i;
                i += 1;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80)
                {
                    i += 1;
                }
                if !skipping && let Some(t) = src.get(start..i) {
                    out.push(t);
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// The index just past the string or character literal whose opening quote
/// is at `q`, with its prefix (`$`, `@`, `$@`, `@$`) read from just before
/// it. Interpolation holes are skipped as code, nested literals included,
/// to a depth of [`MAX_DEPTH`]. Never past the end of `b`.
fn skip_literal(b: &[u8], q: usize, depth: usize) -> usize {
    let quote = b[q];
    let mut k = q;
    let (mut verbatim, mut interpolated) = (false, false);
    while k > 0 && matches!(b[k - 1], b'$' | b'@') {
        k -= 1;
        if b[k] == b'@' {
            verbatim = true;
        } else {
            interpolated = true;
        }
    }
    // `"""…"""` raw strings (not after `@`, where `""` is an escaped quote).
    if quote == b'"' && !verbatim && b.get(q + 1) == Some(&b'"') && b.get(q + 2) == Some(&b'"') {
        let mut n = 0;
        while b.get(q + n) == Some(&b'"') {
            n += 1;
        }
        let mut i = q + n;
        while i < b.len() {
            if (0..n).all(|m| b.get(i + m) == Some(&b'"')) {
                return i + n;
            }
            i += 1;
        }
        return b.len();
    }
    let mut i = q + 1;
    while i < b.len() {
        let c = b[i];
        if c == quote {
            if verbatim && b.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        if c == b'\\' && !verbatim {
            i += 2;
            continue;
        }
        if c == b'\n' && !verbatim {
            // An unterminated literal ends at the line.
            return i;
        }
        if interpolated && c == b'{' {
            if b.get(i + 1) == Some(&b'{') {
                i += 2;
                continue;
            }
            // A hole: code to the matching `}`, nested literals skipped.
            let mut open = 1usize;
            i += 1;
            while i < b.len() && open > 0 {
                match b[i] {
                    b'{' => open += 1,
                    b'}' => open -= 1,
                    b'"' | b'\'' if depth < MAX_DEPTH => {
                        i = skip_literal(b, i, depth + 1);
                        continue;
                    }
                    _ => {}
                }
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ns(src: &str) -> Vec<String> {
        namespaces_of(src.as_bytes())
            .into_iter()
            .map(|n| n.join("."))
            .collect()
    }

    #[test]
    fn block_nested_and_file_scoped_namespaces_are_found() {
        assert_eq!(
            ns("namespace A { namespace B.C { } class X {} }"),
            ["A", "A.B.C"]
        );
        assert_eq!(
            ns("using X;\nnamespace App.Core;\nclass K {}"),
            ["App.Core"]
        );
        assert_eq!(ns("namespace A { } namespace D { }"), ["A", "D"]);
        assert_eq!(ns("class NoNamespace {}"), Vec::<String>::new());
    }

    #[test]
    fn comments_strings_and_directives_hide_namespaces() {
        let src = "// namespace Fake1 {\n/* namespace Fake2 { */\nvar s = \"namespace Fake3 {\";\nvar v = @\"a \"\" namespace Fake4 {\";\nvar r = \"\"\"namespace Fake5 {\"\"\";\n#region namespace Fake6\nnamespace Real { }\n";
        assert_eq!(ns(src), ["Real"]);
    }

    #[test]
    fn verbatim_interpolated_and_conditional_code_keep_nesting_right() {
        assert_eq!(
            ns("namespace A { class C { string q = @\"\"\"\"; } }\nnamespace B { class D {} }"),
            ["A", "B"]
        );
        assert_eq!(
            ns(
                "namespace A { class C { var s = $\"{x.Replace(\"{\", \"\")}\"; } }\nnamespace B { }"
            ),
            ["A", "B"]
        );
        assert_eq!(
            ns("namespace A { class C { var s = $@\"\"\"{x}\"; } }\nnamespace B { }"),
            ["A", "B"]
        );
        assert_eq!(
            ns("class C { void M() { var @namespace = Get(); } }"),
            Vec::<String>::new()
        );
        assert_eq!(
            ns("namespace A { void M() { if (@namespace == y) { } } }"),
            ["A"]
        );
        assert_eq!(
            ns(
                "#if X\nnamespace X {\n#else\nnamespace Y {\n#endif\nclass C {}\n}\nnamespace Z { }"
            ),
            ["X", "Z"]
        );
        assert_eq!(ns("char a = '{'; char b = '\\''; namespace A { }"), ["A"]);
    }

    #[test]
    fn long_and_many_namespaces_stay_linear() {
        let start = std::time::Instant::now();
        let long = format!("namespace {};", vec!["a"; 20_000].join("."));
        assert!(
            namespaces_of(long.as_bytes()).is_empty(),
            "too deep to read"
        );
        let many = format!(
            "namespace {};\n{}",
            vec!["a"; 60].join("."),
            "namespace b { }\n".repeat(20_000)
        );
        assert_eq!(namespaces_of(many.as_bytes()).len(), 2);
        assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
    }

    #[test]
    fn hostile_input_never_panics() {
        for src in [
            "namespace",
            "namespace {",
            "\"unterminated",
            "/* open",
            "'",
            "@\"",
            "\"\"\"x",
            "namespace A.",
            "}}}}{",
        ] {
            let _ = namespaces_of(src.as_bytes());
        }
        let deep = "namespace a {".repeat(10_000);
        assert!(namespaces_of(deep.as_bytes()).len() <= MAX_DEPTH);
        let _ = namespaces_of(&[0xff, 0xfe, b'"', 0x80]);
    }
}
