//! L1 for Go: one system per `go.mod`.
//!
//! `go.mod` and `go.work` are read line by line (single directives, blocks,
//! quotes and `//` comments); the `go` tool is never run (§5.5). Like the go
//! tool, files under `testdata`, `vendor`, or a directory starting with `_`
//! or `.` are ignored.
//!
//! - **Nodes:** every `go.mod`, named by its `module` path. One without a
//!   `module` line, or that is not UTF-8, still becomes a node with
//!   `unknown` confidence.
//! - **Edges:** from direct `require`s (`// indirect` ones are transitive).
//!   The first rule that applies decides:
//!   1. a `replace` in the `go.work` that uses the requirer, then one in the
//!      requirer's own `go.mod`, for that module (and version, if the
//!      replace names one). A local target (`./`, `../`, `.` or `..`) that
//!      holds a `go.mod` gives an `exact` edge; any other target means the
//!      dependency comes from elsewhere, so there is no edge;
//!   2. a module that the requirer's `go.work` `use`s: `exact`;
//!   3. a module path that matches exactly one module in the repository:
//!      `inferred`.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};

use super::path::{Dir, normalise, repo_path};
use super::{Diagnostic, Read};

const MANIFEST: &str = "go.mod";
const WORK: &str = "go.work";

/// Where a `replace` sends a module.
#[derive(Debug, Clone)]
enum Target {
    /// A directory in the repository.
    Local(Dir),
    /// Another module version, or a path outside the repository.
    Elsewhere,
}

#[derive(Debug, Clone)]
struct Replace {
    old: String,
    /// The version the replace applies to; `None` for all versions.
    version: Option<String>,
    target: Target,
}

struct Module {
    dir: Dir,
    manifest: RepoPath,
    /// The `module` path; `None` if the file could not be understood.
    path: Option<String>,
    /// `(module path, version)` of each direct requirement.
    requires: Vec<(String, String)>,
    replaces: Vec<Replace>,
}

struct Work {
    root: Dir,
    file: RepoPath,
    uses: BTreeSet<Dir>,
    replaces: Vec<Replace>,
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Go,
        dir: repo_path(dir),
    })
}

/// Whether the go tool would skip this file's directory.
pub(crate) fn ignored(path: &RepoPath) -> bool {
    let segs = path.segments();
    segs[..segs.len().saturating_sub(1)]
        .iter()
        .any(|s| s == "testdata" || s == "vendor" || s.starts_with('_') || s.starts_with('.'))
}

/// One directive with its arguments, from a single line (`require a v1`) or
/// a block (`require ( .. )`), comments removed.
struct Line {
    verb: String,
    args: Vec<String>,
    indirect: bool,
}

fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '`').to_owned()
}

/// The directives of a `go.mod` or `go.work`, and whether a block was left
/// open at the end of the file.
fn directives(text: &str) -> (Vec<Line>, bool) {
    let mut out = Vec::new();
    let mut block: Option<String> = None;
    for raw in text.lines() {
        let (code, comment) = match raw.find("//") {
            Some(i) => (&raw[..i], &raw[i + 2..]),
            None => (raw, ""),
        };
        let c = comment.trim();
        let indirect = c == "indirect" || c.starts_with("indirect;");
        // `(` and `)` are tokens of their own: `require(` opens a block.
        let spaced = code.replace('(', " ( ").replace(')', " ) ");
        let words: Vec<&str> = spaced.split_whitespace().collect();
        let line = |verb: &str, args: &[&str]| Line {
            verb: verb.to_owned(),
            args: args.iter().map(|a| unquote(a)).collect(),
            indirect,
        };
        match (&block, words.as_slice()) {
            (_, []) => {}
            (Some(_), [")"]) => block = None,
            (Some(verb), args) => out.push(line(verb, args)),
            (None, [verb, "("]) => block = Some((*verb).to_owned()),
            (None, [verb, args @ ..]) => out.push(line(verb, args)),
        }
    }
    (out, block.is_some())
}

/// `.`, `..`, or a path starting with `./`, `../`, `.\` or `..\`, as the
/// go tool's `IsDirectoryPath` decides.
fn is_local(p: &str) -> bool {
    p == "."
        || p == ".."
        || ["./", "../", ".\\", "..\\"]
            .iter()
            .any(|pre| p.starts_with(pre))
}

/// `replace old [v] => new [v]`, with local targets resolved against `dir`.
fn parse_replace(args: &[String], dir: &[String]) -> Option<Replace> {
    let arrow = args.iter().position(|a| a == "=>")?;
    let old = args.first()?.clone();
    let version = (arrow == 2).then(|| args[1].clone());
    let new = args.get(arrow + 1)?;
    let target = if is_local(new) {
        normalise(dir, new).map_or(Target::Elsewhere, Target::Local)
    } else {
        Target::Elsewhere
    };
    Some(Replace {
        old,
        version,
        target,
    })
}

fn parse_module(dir: Dir, manifest: &RepoPath, text: &str) -> (Module, bool) {
    let mut m = Module {
        dir,
        manifest: manifest.clone(),
        path: None,
        requires: Vec::new(),
        replaces: Vec::new(),
    };
    let (lines, open) = directives(text);
    for line in lines {
        match (line.verb.as_str(), line.args.as_slice()) {
            ("module", [p, ..]) => m.path = Some(p.clone()),
            ("require", [p, v, ..]) if !line.indirect => m.requires.push((p.clone(), v.clone())),
            ("require", [p]) if !line.indirect => m.requires.push((p.clone(), String::new())),
            ("replace", args) => m.replaces.extend(parse_replace(args, &m.dir)),
            _ => {}
        }
    }
    (m, open)
}

/// The replace for `module` at `version`, if one applies.
fn find_replace<'r>(replaces: &'r [Replace], module: &str, version: &str) -> Option<&'r Replace> {
    replaces
        .iter()
        .find(|r| r.old == module && r.version.as_deref() == Some(version))
        .or_else(|| {
            replaces
                .iter()
                .find(|r| r.old == module && r.version.is_none())
        })
}

/// Adds the Go systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut modules: Vec<Module> = Vec::new();
    let mut works: Vec<Work> = Vec::new();
    for path in paths.iter().filter(|p| !ignored(p)) {
        let segs = path.segments();
        let Some(file) = segs.last() else { continue };
        if file != MANIFEST && file != WORK {
            continue;
        }
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        let text = read(path).and_then(|b| String::from_utf8(b).ok());
        let diag = |message: &str, diags: &mut Vec<Diagnostic>| {
            diags.push(Diagnostic {
                path: path.clone(),
                message: message.into(),
            })
        };
        if text.is_none() {
            diag("cannot be read as UTF-8", &mut diags);
        }
        let text = text.unwrap_or_default();
        if file == WORK {
            let (lines, open) = directives(&text);
            if open {
                diag("block not closed", &mut diags);
            }
            let mut work = Work {
                root: dir.clone(),
                file: path.clone(),
                uses: BTreeSet::new(),
                replaces: Vec::new(),
            };
            for line in lines {
                match (line.verb.as_str(), line.args.as_slice()) {
                    ("use", [p, ..]) => match normalise(&dir, p) {
                        Some(d) => {
                            work.uses.insert(d);
                        }
                        None => diag(&format!("use {p} is outside the repository"), &mut diags),
                    },
                    ("replace", args) => work.replaces.extend(parse_replace(args, &dir)),
                    _ => {}
                }
            }
            works.push(work);
            continue;
        }
        let (m, open) = parse_module(dir, path, &text);
        if open {
            diag("block not closed", &mut diags);
        }
        if m.path.is_none() && !text.is_empty() {
            diag("no module directive", &mut diags);
        }
        modules.push(m);
    }

    let by_dir: BTreeMap<&[String], usize> = modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.dir.as_slice(), i))
        .collect();
    let mut by_path: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, m) in modules.iter().enumerate() {
        if let Some(p) = &m.path {
            by_path.entry(p.as_str()).or_default().push(i);
        }
    }
    for w in &works {
        for u in &w.uses {
            if !by_dir.contains_key(u.as_slice()) {
                diags.push(Diagnostic {
                    path: w.file.clone(),
                    message: format!("use {} has no go.mod", repo_path(u)),
                });
            }
        }
    }

    for m in &modules {
        let fallback = m.dir.last().cloned().unwrap_or_else(|| "(root)".to_owned());
        let node = Node {
            id: node_id(&m.dir),
            kind: NodeKind::System,
            name: m.path.clone().unwrap_or(fallback),
            source: m.manifest.clone(),
            parent: None,
            confidence: if m.path.is_some() {
                Confidence::Exact
            } else {
                Confidence::Unknown
            },
            visibility: None,
            code: None,
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: m.manifest.clone(),
                message: e.to_string(),
            });
        }
    }

    let mut edges: BTreeMap<(usize, usize), Confidence> = BTreeMap::new();
    for (from, m) in modules.iter().enumerate() {
        // The nearest go.work that uses this module.
        let work = works
            .iter()
            .filter(|w| w.uses.contains(&m.dir))
            .max_by_key(|w| w.root.len());
        for (req, version) in &m.requires {
            let replace = work
                .and_then(|w| find_replace(&w.replaces, req, version))
                .or_else(|| find_replace(&m.replaces, req, version));
            let found = match replace.map(|r| &r.target) {
                Some(Target::Local(dir)) => match by_dir.get(dir.as_slice()) {
                    Some(&i) => Some((i, Confidence::Exact)),
                    None => {
                        diags.push(Diagnostic {
                            path: m.manifest.clone(),
                            message: format!(
                                "replace for {req} points to {}, which has no go.mod",
                                repo_path(dir)
                            ),
                        });
                        None
                    }
                },
                Some(Target::Elsewhere) => None,
                None => {
                    let candidates = by_path.get(req.as_str()).cloned().unwrap_or_default();
                    let used: Vec<usize> = candidates
                        .iter()
                        .copied()
                        .filter(|&c| work.is_some_and(|w| w.uses.contains(&modules[c].dir)))
                        .collect();
                    match (used.as_slice(), candidates.as_slice()) {
                        ([one], _) => Some((*one, Confidence::Exact)),
                        ([], [one]) => Some((*one, Confidence::Inferred)),
                        _ => None,
                    }
                }
            };
            if let Some((to, conf)) = found
                && to != from
            {
                let e = edges.entry((from, to)).or_insert(conf);
                *e = (*e).max(conf);
            }
        }
    }
    for ((from, to), confidence) in edges {
        let edge = Edge {
            key: EdgeKey {
                from: node_id(&modules[from].dir),
                to: node_id(&modules[to].dir),
                kind: EdgeKind::DependsOn,
            },
            confidence,
            evidence: BTreeSet::new(),
            dependency: BTreeSet::from([DepKind::Normal]),
        };
        if let Err(e) = model.add_edge(edge) {
            diags.push(Diagnostic {
                path: modules[from].manifest.clone(),
                message: e.to_string(),
            });
        }
    }
    diags.sort();
    diags.dedup();
    diags
}
