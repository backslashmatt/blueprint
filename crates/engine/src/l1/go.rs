//! L1 for Go: one system per `go.mod`.
//!
//! `go.mod` and `go.work` are read line by line; the `go` tool is never run
//! (§5.5).
//!
//! - **Nodes:** every `go.mod`, named by its `module` path. One without a
//!   `module` line, or that is not UTF-8, still becomes a node with
//!   `unknown` confidence.
//! - **Edges:** from direct `require`s (`// indirect` ones are transitive):
//!   - `exact`: a `replace` to a local directory (`./` or `../`) that holds
//!     a `go.mod`, or a module that a `go.work` covering the requirer `use`s;
//!   - `inferred`: a module path that matches another module in the
//!     repository and nothing above says otherwise.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};

use super::path::{Dir, normalise, repo_path};
use super::{Diagnostic, Read};

const MANIFEST: &str = "go.mod";
const WORK: &str = "go.work";

struct Module {
    dir: Dir,
    manifest: RepoPath,
    /// The `module` path; `None` if the file could not be understood.
    path: Option<String>,
    requires: Vec<String>,
    /// `replace` targets that are local directories, by module path.
    replaces: BTreeMap<String, Dir>,
}

struct Work {
    root: Dir,
    uses: BTreeSet<Dir>,
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Go,
        dir: repo_path(dir),
    })
}

/// One directive with its arguments, from either a single line
/// (`require a v1`) or a block (`require ( .. )`), comments removed.
/// `indirect` is true when the line carried `// indirect`.
struct Line {
    verb: String,
    args: Vec<String>,
    indirect: bool,
}

fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '`').to_owned()
}

fn directives(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut block: Option<String> = None;
    for raw in text.lines() {
        let (code, comment) = match raw.find("//") {
            Some(i) => (&raw[..i], &raw[i + 2..]),
            None => (raw, ""),
        };
        let indirect = comment.trim() == "indirect";
        let words: Vec<&str> = code.split_whitespace().collect();
        match (&block, words.as_slice()) {
            (_, []) => {}
            (Some(_), [")"]) => block = None,
            (Some(verb), args) => out.push(Line {
                verb: verb.clone(),
                args: args.iter().map(|a| unquote(a)).collect(),
                indirect,
            }),
            (None, [verb, "("]) => block = Some((*verb).to_owned()),
            (None, [verb, args @ ..]) => out.push(Line {
                verb: (*verb).to_owned(),
                args: args.iter().map(|a| unquote(a)).collect(),
                indirect,
            }),
        }
    }
    out
}

fn parse_module(dir: Dir, manifest: &RepoPath, text: &str) -> Module {
    let mut m = Module {
        dir,
        manifest: manifest.clone(),
        path: None,
        requires: Vec::new(),
        replaces: BTreeMap::new(),
    };
    for line in directives(text) {
        match (line.verb.as_str(), line.args.as_slice()) {
            ("module", [p, ..]) => m.path = Some(p.clone()),
            ("require", [p, ..]) if !line.indirect => m.requires.push(p.clone()),
            // `replace old [v] => new [v]`
            ("replace", args) => {
                if let Some(arrow) = args.iter().position(|a| a == "=>")
                    && let (Some(old), Some(new)) = (args.first(), args.get(arrow + 1))
                    && (new.starts_with("./") || new.starts_with("../"))
                    && let Some(target) = normalise(&m.dir, new)
                {
                    m.replaces.insert(old.clone(), target);
                }
            }
            _ => {}
        }
    }
    m
}

/// Adds the Go systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut modules: Vec<Module> = Vec::new();
    let mut works: Vec<Work> = Vec::new();
    for path in paths {
        let segs = path.segments();
        let Some(file) = segs.last() else { continue };
        if file != MANIFEST && file != WORK {
            continue;
        }
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        let text = read(path).and_then(|b| String::from_utf8(b).ok());
        if file == WORK {
            let uses = directives(text.as_deref().unwrap_or(""))
                .into_iter()
                .filter(|l| l.verb == "use")
                .filter_map(|l| l.args.first().and_then(|a| normalise(&dir, a)))
                .collect();
            works.push(Work { root: dir, uses });
            continue;
        }
        let m = parse_module(dir, path, text.as_deref().unwrap_or(""));
        if text.is_none() {
            diags.push(Diagnostic {
                path: path.clone(),
                message: "cannot be read as UTF-8".into(),
            });
        } else if m.path.is_none() {
            diags.push(Diagnostic {
                path: path.clone(),
                message: "no module directive".into(),
            });
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
        for req in &m.requires {
            let found = if let Some(target) = m.replaces.get(req) {
                match by_dir.get(target.as_slice()) {
                    Some(&i) => Some((i, Confidence::Exact)),
                    None => {
                        diags.push(Diagnostic {
                            path: m.manifest.clone(),
                            message: format!(
                                "replace for {req} points to {}, which has no go.mod",
                                repo_path(target)
                            ),
                        });
                        None
                    }
                }
            } else {
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
