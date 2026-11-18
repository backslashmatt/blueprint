//! L1 for Bazel: one system per package (a directory with `BUILD` or
//! `BUILD.bazel`).
//!
//! BUILD files are tokenised as Starlark (strings, comments, brackets);
//! `bazel` is never run and nothing is evaluated (§5.5).
//!
//! - **Nodes:** every package, named by its label (`//path/to/pkg`). If
//!   both files exist, `BUILD.bazel` is read, as Bazel does. A file that is
//!   not UTF-8 or has unbalanced brackets or quotes still becomes a node,
//!   with `unknown` confidence, and its edges are only `inferred`.
//! - **Repositories:** a directory with `MODULE.bazel`, `WORKSPACE` or
//!   `WORKSPACE.bazel` starts a repository; `//` labels resolve within the
//!   repository they are written in, and a bzlmod `module(name = ..)` makes
//!   `@name//` refer to it too. Paths in `.bazelignore` are skipped.
//! - **Edges:** string labels inside dependency attributes (`deps`,
//!   `runtime_deps`, `exports`, `implementation_deps`, `data`, `srcs`,
//!   `hdrs`, `tools`, `plugins`), including inside `select()` values but not
//!   its condition keys, and not strings joined with `+`, `%` or
//!   `.format`, which are not literal. A label in the same repository (`//pkg:t`,
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
    /// `+`, `%` or `.`: a neighbouring string is being built, not literal.
    Join,
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
            '+' | '%' | '.' => out.push(Tok::Join),
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
            // Attributes are keyword arguments, so inside a call (depth >= 1).
            Tok::Ident(name)
                if depth >= 1
                    && reading.is_none()
                    && DEP_ATTRS.contains(&name.as_str())
                    && toks.get(k + 1) == Some(&Tok::Eq) =>
            {
                reading = Some(depth);
            }
            // A string followed by `:` is a select() condition key; one next
            // to `+`, `%` or `.` is being built, so it is not a literal label.
            Tok::Str(s)
                if reading.is_some()
                    && !matches!(toks.get(k + 1), Some(Tok::Colon | Tok::Join))
                    && (k == 0 || toks[k - 1] != Tok::Join) =>
            {
                out.push(s.clone())
            }
            _ => {}
        }
    }
    (out, depth == 0)
}

/// The package path a label points to inside the repository it is written
/// in; `None` for local labels and other repositories. `module` is the
/// repository's own bzlmod name, which also refers to itself.
fn label_package<'l>(label: &'l str, module: Option<&str>) -> Option<&'l str> {
    let rest = label
        .strip_prefix("@@//")
        .or_else(|| label.strip_prefix("@//"))
        .or_else(|| label.strip_prefix("//"))
        .or_else(|| {
            let m = module?;
            let after = label
                .strip_prefix("@@")
                .or_else(|| label.strip_prefix('@'))?;
            after.strip_prefix(m)?.strip_prefix("//")
        })?;
    Some(rest.split(':').next().unwrap_or(""))
}

/// Files that make their directory the root of a repository.
const BOUNDARIES: [&str; 3] = ["MODULE.bazel", "WORKSPACE", "WORKSPACE.bazel"];

/// `module(name = "..")` in a `MODULE.bazel`.
fn module_name(text: &str) -> Option<String> {
    let toks = tokens(text)?;
    toks.windows(5).find_map(|w| match w {
        [
            Tok::Ident(m),
            Tok::Open,
            Tok::Ident(n),
            Tok::Eq,
            Tok::Str(v),
        ] if m == "module" && n == "name" => Some(v.clone()),
        _ => None,
    })
}

struct Repo {
    root: Dir,
    module: Option<String>,
}

/// Adds the Bazel systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let text_of = |p: &RepoPath| read(p).and_then(|b| String::from_utf8(b).ok());
    // Repositories: the top level, and any directory with a boundary file.
    let mut repos: Vec<Repo> = vec![Repo {
        root: Vec::new(),
        module: None,
    }];
    let mut ignored: Vec<Dir> = Vec::new();
    for path in paths {
        let segs = path.segments();
        let Some(file) = segs.last() else { continue };
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        if BOUNDARIES.contains(&file.as_str()) {
            let module = if file == "MODULE.bazel" {
                text_of(path).as_deref().and_then(module_name)
            } else {
                None
            };
            match repos.iter_mut().find(|r| r.root == dir) {
                Some(r) => r.module = r.module.take().or(module),
                None => repos.push(Repo {
                    root: dir.clone(),
                    module,
                }),
            }
        }
        if file == ".bazelignore" {
            for line in text_of(path).unwrap_or_default().lines() {
                let line = line.trim();
                if !line.is_empty() && !line.starts_with('#') {
                    ignored.extend(normalise(&dir, line));
                }
            }
        }
    }
    let repo_of = |dir: &[String]| -> usize {
        repos
            .iter()
            .enumerate()
            .filter(|(_, r)| dir.starts_with(&r.root))
            .max_by_key(|(_, r)| r.root.len())
            .map_or(0, |(i, _)| i)
    };

    // BUILD.bazel wins over BUILD in the same directory.
    let mut files: BTreeMap<Dir, &RepoPath> = BTreeMap::new();
    for path in paths {
        let segs = path.segments();
        let Some(file) = segs.last() else { continue };
        if file != "BUILD" && file != "BUILD.bazel" {
            continue;
        }
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        if ignored.iter().any(|i| dir.starts_with(i)) {
            continue;
        }
        if file == "BUILD.bazel" || !files.contains_key(&dir) {
            files.insert(dir, path);
        }
    }
    let mut packages: Vec<Package> = Vec::new();
    for (dir, path) in files {
        let text = text_of(path);
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
        let repo = &repos[repo_of(&p.dir)];
        let rel = p.dir[repo.root.len()..].join("/");
        let name = if repo.root.is_empty() {
            format!("//{rel}")
        } else {
            format!("{}//{rel}", repo.root.join("/"))
        };
        let node = Node {
            id: node_id(&p.dir),
            kind: NodeKind::System,
            name,
            source: p.file.clone(),
            parent: None,
            confidence: if p.ok {
                Confidence::Exact
            } else {
                Confidence::Unknown
            },
            visibility: None,
            code: None,
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: p.file.clone(),
                message: e.to_string(),
            });
        }
    }

    let mut edges: BTreeMap<(usize, usize), Confidence> = BTreeMap::new();
    for (from, p) in packages.iter().enumerate() {
        let repo = &repos[repo_of(&p.dir)];
        // Labels read from a damaged file are only a best guess.
        let conf = if p.ok {
            Confidence::Exact
        } else {
            Confidence::Inferred
        };
        for label in &p.labels {
            let Some(pkg) = label_package(label, repo.module.as_deref()) else {
                continue;
            };
            let Some(dir) = normalise(&repo.root, pkg).filter(|_| !pkg.contains("..")) else {
                diags.push(Diagnostic {
                    path: p.file.clone(),
                    message: format!("label {label} is not a valid package path"),
                });
                continue;
            };
            match by_dir.get(dir.as_slice()) {
                Some(&to) if to != from => {
                    let e = edges.entry((from, to)).or_insert(conf);
                    *e = (*e).max(conf);
                }
                Some(_) => {}
                None => diags.push(Diagnostic {
                    path: p.file.clone(),
                    message: format!("label {label} names no package"),
                }),
            }
        }
    }
    for ((from, to), confidence) in edges {
        let edge = Edge {
            key: EdgeKey {
                from: node_id(&packages[from].dir),
                to: node_id(&packages[to].dir),
                kind: EdgeKind::DependsOn,
            },
            confidence,
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
