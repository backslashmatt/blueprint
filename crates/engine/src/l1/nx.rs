//! L1 for Nx: one system per `project.json`.
//!
//! Nx derives most project dependencies by analysing imports when it runs;
//! Blueprint never runs `nx` (§5.5). What `project.json` states literally is
//! `implicitDependencies`, so that is what makes edges here. Files under
//! `node_modules` are ignored.
//!
//! - **Nodes:** every `project.json`, named by its `name` (else its
//!   directory). Malformed JSON still becomes a node, with `unknown`
//!   confidence.
//! - **Edges:** each `implicitDependencies` entry:
//!   - `exact`: a project name that matches exactly one project;
//!   - `inferred`: each project a glob entry (`*`, `?`) matches;
//!   - a `!name` entry removes that project from the list (Nx's negation).

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};
use serde_json::Value;

use super::path::{Dir, Glob, repo_path};
use super::{Diagnostic, Read};

const MANIFEST: &str = "project.json";

struct Project {
    dir: Dir,
    file: RepoPath,
    name: String,
    ok: bool,
    implicit: Vec<String>,
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Nx,
        dir: repo_path(dir),
    })
}

/// Adds the Nx systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut projects: Vec<Project> = Vec::new();
    for path in paths {
        let segs = path.segments();
        if segs.last().is_none_or(|f| f != MANIFEST) || segs.iter().any(|s| s == "node_modules") {
            continue;
        }
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        let fallback = dir.last().cloned().unwrap_or_else(|| "(root)".to_owned());
        let parsed = read(path)
            .and_then(|b| String::from_utf8(b).ok())
            .map(|t| t.strip_prefix('\u{feff}').map(str::to_owned).unwrap_or(t))
            .ok_or_else(|| "cannot be read as UTF-8".to_owned())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
            .and_then(|v| {
                if v.is_object() {
                    Ok(v)
                } else {
                    Err("is not a JSON object".to_owned())
                }
            });
        match parsed {
            Err(message) => {
                diags.push(Diagnostic {
                    path: path.clone(),
                    message,
                });
                projects.push(Project {
                    dir,
                    file: path.clone(),
                    name: fallback,
                    ok: false,
                    implicit: Vec::new(),
                });
            }
            Ok(json) => {
                let name = json
                    .get("name")
                    .and_then(Value::as_str)
                    .map_or(fallback, str::to_owned);
                let implicit = json
                    .get("implicitDependencies")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                projects.push(Project {
                    dir,
                    file: path.clone(),
                    name,
                    ok: true,
                    implicit,
                });
            }
        }
    }

    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, p) in projects.iter().enumerate() {
        by_name.entry(p.name.as_str()).or_default().push(i);
    }
    for p in &projects {
        let node = Node {
            id: node_id(&p.dir),
            kind: NodeKind::System,
            name: p.name.clone(),
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

    let mut edges: BTreeMap<(usize, usize), Confidence> = BTreeMap::new();
    for (from, p) in projects.iter().enumerate() {
        let mut targets: BTreeMap<usize, Confidence> = BTreeMap::new();
        let mut removed: BTreeSet<usize> = BTreeSet::new();
        for entry in &p.implicit {
            if let Some(neg) = entry.strip_prefix('!') {
                removed.extend(by_name.get(neg).into_iter().flatten().copied());
                continue;
            }
            if entry.contains(['*', '?']) {
                // A project name is one segment, so a one-segment glob.
                match Glob::compile(&entry.replace('/', "\u{2215}")) {
                    Ok(Some(g)) => {
                        for (i, q) in projects.iter().enumerate() {
                            if g.matches(&[q.name.replace('/', "\u{2215}")]) {
                                targets.entry(i).or_insert(Confidence::Inferred);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(message) => diags.push(Diagnostic {
                        path: p.file.clone(),
                        message,
                    }),
                }
                continue;
            }
            match by_name.get(entry.as_str()).map(Vec::as_slice) {
                Some([one]) => {
                    targets.insert(*one, Confidence::Exact);
                }
                Some(_) => diags.push(Diagnostic {
                    path: p.file.clone(),
                    message: format!("implicit dependency {entry} names several projects"),
                }),
                None => diags.push(Diagnostic {
                    path: p.file.clone(),
                    message: format!("implicit dependency {entry} names no project"),
                }),
            }
        }
        for (to, conf) in targets {
            if to != from && !removed.contains(&to) {
                let e = edges.entry((from, to)).or_insert(conf);
                *e = (*e).max(conf);
            }
        }
    }
    for ((from, to), confidence) in edges {
        let edge = Edge {
            key: EdgeKey {
                from: node_id(&projects[from].dir),
                to: node_id(&projects[to].dir),
                kind: EdgeKind::DependsOn,
            },
            confidence,
            evidence: BTreeSet::new(),
            dependency: BTreeSet::from([DepKind::Normal]),
        };
        if let Err(e) = model.add_edge(edge) {
            diags.push(Diagnostic {
                path: projects[from].file.clone(),
                message: e.to_string(),
            });
        }
    }
    diags.sort();
    diags.dedup();
    diags
}
