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
/// paths: `namespace A { namespace B.C {} }` gives `A` and `A.B.C`.
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
            "namespace" => {
                // A dotted name, then `{` (block) or `;` (file-scoped).
                let mut name = Vec::new();
                let mut j = i + 1;
                while let Some(t) = tokens.get(j) {
                    if is_ident(t) {
                        name.push((*t).to_owned());
                        j += 1;
                        if tokens.get(j) == Some(&".") {
                            j += 1;
                            continue;
                        }
                    }
                    break;
                }
                let outer = stack
                    .last()
                    .map_or_else(|| file_scoped.clone(), |(p, _)| p.clone());
                let full = [outer, name.clone()].concat();
                match tokens.get(j) {
                    Some(&"{") if !name.is_empty() && full.len() <= MAX_DEPTH => {
                        depth += 1;
                        stack.push((full.clone(), depth));
                        out.insert(full);
                        i = j;
                    }
                    Some(&";") if !name.is_empty() && stack.is_empty() => {
                        file_scoped = name;
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

/// Identifiers and the punctuation `{ } ; .`, with comments, strings,
/// characters and preprocessor lines skipped.
fn tokens(src: &str) -> Vec<&str> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                line_start = true;
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            // `#region`, `#if` and the like: to the end of the line.
            b'#' if line_start => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            b'"' if b.get(i + 1) == Some(&b'"') && b.get(i + 2) == Some(&b'"') => {
                // A raw string: to the next run of as many quotes.
                let mut n = 0;
                while b.get(i + n) == Some(&b'"') {
                    n += 1;
                }
                i += n;
                'raw: while i < b.len() {
                    if (0..n).all(|k| b.get(i + k) == Some(&b'"')) {
                        i += n;
                        break 'raw;
                    }
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                // `@"…"` and `$@"…"` double their quotes; others escape.
                let verbatim = i > 0 && (b[i - 1] == b'@' || (i > 1 && b[i - 2] == b'@'));
                i += 1;
                while i < b.len() {
                    if b[i] == c {
                        if verbatim && b.get(i + 1) == Some(&c) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    if b[i] == b'\\' && !verbatim {
                        i += 1;
                    }
                    if b[i.min(b.len() - 1)] == b'\n' && c == b'\'' {
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'{' | b'}' | b';' | b'.' => {
                out.push(&src[i..=i]);
                i += 1;
            }
            _ if c.is_ascii_alphabetic() || c == b'_' || c == b'@' || c >= 0x80 => {
                let start = i;
                while i < b.len()
                    && (b[i].is_ascii_alphanumeric()
                        || b[i] == b'_'
                        || b[i] == b'@'
                        || b[i] >= 0x80)
                {
                    i += 1;
                }
                if let Some(t) = src.get(start..i) {
                    out.push(t.trim_start_matches('@'));
                }
            }
            _ => i += 1,
        }
        line_start = false;
    }
    out
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
