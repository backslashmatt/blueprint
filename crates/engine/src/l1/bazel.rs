//! L1 for Bazel: one system per package (a directory with `BUILD` or
//! `BUILD.bazel`).
//!
//! BUILD files are tokenised as Starlark (strings, comments, brackets);
//! `bazel` is never run and nothing is evaluated (§5.5).
//!
//! - **Nodes:** every package, named by its label (`//path/to/pkg`). If
//!   both files exist, `BUILD.bazel` is read, as Bazel does. A file that is
//!   not UTF-8 or has unbalanced brackets or quotes still becomes a node,
//!   with `unknown` confidence.
//! - **Edges:** string labels inside dependency attributes (`deps`,
//!   `runtime_deps`, `exports`, `implementation_deps`, `data`, `srcs`,
//!   `hdrs`, `tools`, `plugins`), including inside `select()` values but not
//!   its condition keys. A label in the main repository (`//pkg:t`,
//!   `@//pkg`, `@@//pkg`) is stated literally, so its edge is `exact`.
//!   Labels of other repositories (`@x//..`) are external; local labels
//!   (`:t`, `t`) stay inside the package. Dependencies built by macros or
//!   variables are not visible here and so produce no edge.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};

use super::path::{Dir, normalise, repo_path};
use super::{Diagnostic, Read};

const DEP_ATTRS: [&str; 9] = [
    "deps",
    "runtime_deps",
    "exports",
    "implementation_deps",
    "data",
    "srcs",
    "hdrs",
    "tools",
    "plugins",
];

struct Package {
    dir: Dir,
    file: RepoPath,
    ok: bool,
    labels: Vec<String>,
}

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String),
    Ident(String),
    Open,
    Close,
    Comma,
    Colon,
    Eq,
    Other,
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Bazel,
        dir: repo_path(dir),
    })
}

/// Tokenises Starlark source; `None` if a string is not closed.
fn tokens(src: &str) -> Option<Vec<Tok>> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '#' => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '(' | '[' | '{' => out.push(Tok::Open),
            ')' | ']' | '}' => out.push(Tok::Close),
            ',' => out.push(Tok::Comma),
            ':' => out.push(Tok::Colon),
            '=' if c.get(i + 1) != Some(&'=') => out.push(Tok::Eq),
            '"' | '\'' => {
                let triple = c.get(i + 1) == Some(&ch) && c.get(i + 2) == Some(&ch);
                let raw = i > 0 && matches!(c[i - 1], 'r' | 'R');
                let mut s = String::new();
                i += if triple { 3 } else { 1 };
                loop {
                    let d = *c.get(i)?;
                    if d == '\\' && !raw {
                        s.push(*c.get(i + 1)?);
                        i += 2;
                        continue;
                    }
                    if d == ch
                        && (!triple || (c.get(i + 1) == Some(&ch) && c.get(i + 2) == Some(&ch)))
                    {
                        i += if triple { 3 } else { 1 };
                        break;
                    }
                    if d == '\n' && !triple {
                        return None;
                    }
                    s.push(d);
                    i += 1;
                }
                out.push(Tok::Str(s));
                continue;
            }
            c0 if c0.is_alphabetic() || c0 == '_' => {
                let start = i;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
                let word: String = c[start..i].iter().collect();
                // A string prefix such as r"..": let the quote start the string.
                if (word == "r" || word == "R") && matches!(c.get(i), Some('"' | '\'')) {
                    continue;
                }
                out.push(Tok::Ident(word));
                continue;
            }
            c0 if c0.is_whitespace() => {}
            _ => out.push(Tok::Other),
        }
        i += 1;
    }
    Some(out)
}

/// The label strings in dependency attributes, and whether brackets balance.
fn labels(toks: &[Tok]) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut depth: i64 = 0;
    // The depth at which a dependency attribute's value is being read.
    let mut reading: Option<i64> = None;
    for (k, t) in toks.iter().enumerate() {
        match t {
            Tok::Open => depth += 1,
            Tok::Close => {
                depth -= 1;
                if depth < 0 {
                    return (out, false);
                }
                if reading.is_some_and(|d| depth < d) {
                    reading = None;
                }
            }
            Tok::Comma if reading == Some(depth) => reading = None,
            Tok::Ident(name)
                if reading.is_none()
                    && DEP_ATTRS.contains(&name.as_str())
                    && toks.get(k + 1) == Some(&Tok::Eq) =>
            {
                reading = Some(depth);
            }
            // A string followed by `:` is a select() condition key.
            Tok::Str(s) if reading.is_some() && toks.get(k + 1) != Some(&Tok::Colon) => {
                out.push(s.clone())
            }
            _ => {}
        }
    }
    (out, depth == 0)
}

/// The package directory a main-repository label points to; `None` for
/// local or other-repository labels.
fn label_package(label: &str) -> Option<&str> {
    let rest = label
        .strip_prefix("@@//")
        .or_else(|| label.strip_prefix("@//"))
        .or_else(|| label.strip_prefix("//"))?;
    Some(rest.split(':').next().unwrap_or(""))
}

/// Adds the Bazel systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    // BUILD.bazel wins over BUILD in the same directory.
    let mut files: BTreeMap<Dir, &RepoPath> = BTreeMap::new();
    for path in paths {
        let segs = path.segments();
        let Some(file) = segs.last() else { continue };
        if file != "BUILD" && file != "BUILD.bazel" {
            continue;
        }
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        if file == "BUILD.bazel" || !files.contains_key(&dir) {
            files.insert(dir, path);
        }
    }
    let mut packages: Vec<Package> = Vec::new();
    for (dir, path) in files {
        let text = read(path).and_then(|b| String::from_utf8(b).ok());
        let toks = text.as_deref().and_then(tokens);
        let (labels, balanced) = toks.as_deref().map_or((Vec::new(), false), labels);
        let ok = toks.is_some() && balanced;
        if !ok {
            let why = if text.is_none() {
                "cannot be read as UTF-8"
            } else {
                "unbalanced brackets or unclosed string"
            };
            diags.push(Diagnostic {
                path: path.clone(),
                message: why.into(),
            });
        }
        packages.push(Package {
            dir,
            file: path.clone(),
            ok,
            labels,
        });
    }
    let by_dir: BTreeMap<&[String], usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dir.as_slice(), i))
        .collect();

    for p in &packages {
        let node = Node {
            id: node_id(&p.dir),
            kind: NodeKind::System,
            name: format!("//{}", p.dir.join("/")),
            source: p.file.clone(),
            parent: None,
            confidence: if p.ok {
                Confidence::Exact
            } else {
                Confidence::Unknown
            },
            visibility: None,
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: p.file.clone(),
                message: e.to_string(),
            });
        }
    }

    let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (from, p) in packages.iter().enumerate() {
        for label in &p.labels {
            let Some(pkg) = label_package(label) else {
                continue;
            };
            let Some(dir) = normalise(&[], pkg).filter(|_| !pkg.contains("..")) else {
                diags.push(Diagnostic {
                    path: p.file.clone(),
                    message: format!("label {label} is not a valid package path"),
                });
                continue;
            };
            match by_dir.get(dir.as_slice()) {
                Some(&to) if to != from => {
                    edges.insert((from, to));
                }
                Some(_) => {}
                None => diags.push(Diagnostic {
                    path: p.file.clone(),
                    message: format!("label {label} names no package"),
                }),
            }
        }
    }
    for (from, to) in edges {
        let edge = Edge {
            key: EdgeKey {
                from: node_id(&packages[from].dir),
                to: node_id(&packages[to].dir),
                kind: EdgeKind::DependsOn,
            },
            confidence: Confidence::Exact,
            evidence: BTreeSet::new(),
            dependency: BTreeSet::from([DepKind::Normal]),
        };
        if let Err(e) = model.add_edge(edge) {
            diags.push(Diagnostic {
                path: packages[from].file.clone(),
                message: e.to_string(),
            });
        }
    }
    diags.sort();
    diags.dedup();
    diags
}
