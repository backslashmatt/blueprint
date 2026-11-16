//! L1 for Nx: one system per Nx project.
//!
//! Nx derives most project dependencies by analysing imports when it runs;
//! Blueprint never runs `nx` (§5.5). What the project configuration states
//! literally is `implicitDependencies`, so that is what makes edges here.
//! Files under `node_modules` are ignored.
//!
//! - **Projects:** every `project.json`, and every `package.json` with an
//!   `nx` key and no `project.json` beside it. A project is named by its
//!   `name`; failing that, by the `name` of the `package.json` beside it;
//!   failing that, by its lowercased directory name, as Nx does. Malformed
//!   JSON still becomes a node, with `unknown` confidence, but its guessed
//!   name is not used to resolve references.
//! - **Edges:** each `implicitDependencies` entry is a project name, a glob
//!   (`*`, `?`, `[..]`, `{a,b}`), or `tag:` and a tag name or glob; a leading
//!   `!` removes what it matches.
//!   - `exact`: a project name that matches exactly one project;
//!   - `inferred`: each project a glob or `tag:` entry matches.
//! - `workspace.json` (Nx's legacy project list) is not read.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};
use serde_json::Value;

use super::path::{Dir, Glob, repo_path};
use super::{Diagnostic, Read};

struct Project {
    dir: Dir,
    file: RepoPath,
    name: String,
    ok: bool,
    tags: Vec<String>,
    implicit: Vec<String>,
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Nx,
        dir: repo_path(dir),
    })
}

fn json_at(path: &RepoPath, read: Read<'_>) -> Result<Value, String> {
    read(path)
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
        })
}

fn strings(
    v: Option<&Value>,
    what: &str,
    file: &RepoPath,
    diags: &mut Vec<Diagnostic>,
) -> Vec<String> {
    match v {
        None => Vec::new(),
        Some(Value::Array(a)) => {
            if a.iter().any(|x| !x.is_string()) {
                diags.push(Diagnostic {
                    path: file.clone(),
                    message: format!("{what} has entries that are not strings"),
                });
            }
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        }
        Some(_) => {
            diags.push(Diagnostic {
                path: file.clone(),
                message: format!("{what} is not an array"),
            });
            Vec::new()
        }
    }
}

/// A one-segment glob over names, which may themselves contain `/`.
fn name_globs(pattern: &str) -> Result<Vec<Glob>, String> {
    Glob::compile_all(&pattern.replace('/', "\u{2215}"))
}

fn glob_matches(globs: &[Glob], name: &str) -> bool {
    let seg = [name.replace('/', "\u{2215}")];
    globs.iter().any(|g| g.matches(&seg))
}

/// Adds the Nx systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let files: BTreeSet<&RepoPath> = paths
        .iter()
        .filter(|p| !p.segments().iter().any(|s| s == "node_modules"))
        .collect();
    let sibling = |dir: &[String], file: &str| -> Option<RepoPath> {
        let p = repo_path(&[dir, &[file.to_owned()]].concat());
        files.contains(&p).then_some(p)
    };
    let mut projects: Vec<Project> = Vec::new();
    for path in &files {
        let segs = path.segments();
        let Some(file) = segs.last() else { continue };
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        let (config, nx_key) = match file.as_str() {
            "project.json" => (json_at(path, read), false),
            // A package.json is an Nx project only with an `nx` key and no
            // project.json beside it.
            "package.json" if sibling(&dir, "project.json").is_none() => {
                match json_at(path, read) {
                    Ok(v) if v.get("nx").is_some_and(Value::is_object) => (Ok(v), true),
                    _ => continue,
                }
            }
            _ => continue,
        };
        let package_name = || {
            sibling(&dir, "package.json")
                .and_then(|p| json_at(&p, read).ok())
                .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned))
        };
        let dir_name = || {
            dir.last()
                .map_or_else(|| "(root)".to_owned(), |d| d.to_lowercase())
        };
        match config {
            Err(message) => {
                diags.push(Diagnostic {
                    path: (*path).clone(),
                    message,
                });
                projects.push(Project {
                    dir: dir.clone(),
                    file: (*path).clone(),
                    name: dir_name(),
                    ok: false,
                    tags: Vec::new(),
                    implicit: Vec::new(),
                });
            }
            Ok(json) => {
                let nx = if nx_key {
                    json.get("nx").cloned().unwrap_or(Value::Null)
                } else {
                    json.clone()
                };
                let name = nx
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        if nx_key {
                            json.get("name").and_then(Value::as_str).map(str::to_owned)
                        } else {
                            package_name()
                        }
                    })
                    .unwrap_or_else(dir_name);
                let tags = strings(nx.get("tags"), "tags", path, &mut diags);
                let implicit = strings(
                    nx.get("implicitDependencies"),
                    "implicitDependencies",
                    path,
                    &mut diags,
                );
                projects.push(Project {
                    dir: dir.clone(),
                    file: (*path).clone(),
                    name,
                    ok: true,
                    tags,
                    implicit,
                });
            }
        }
    }

    // Only projects that could be read resolve names.
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, p) in projects.iter().enumerate().filter(|(_, p)| p.ok) {
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
            let (negated, pattern) = match entry.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, entry.as_str()),
            };
            let report = |message: String, diags: &mut Vec<Diagnostic>| {
                diags.push(Diagnostic {
                    path: p.file.clone(),
                    message,
                })
            };
            let matched: Vec<(usize, Confidence)> = if let Some(tag) = pattern.strip_prefix("tag:")
            {
                match name_globs(tag) {
                    Ok(g) => (0..projects.len())
                        .filter(|&i| {
                            projects[i].ok && projects[i].tags.iter().any(|t| glob_matches(&g, t))
                        })
                        .map(|i| (i, Confidence::Inferred))
                        .collect(),
                    Err(m) => {
                        report(m, &mut diags);
                        continue;
                    }
                }
            } else if pattern.contains(['*', '?', '[', '{']) {
                match name_globs(pattern) {
                    Ok(g) => (0..projects.len())
                        .filter(|&i| projects[i].ok && glob_matches(&g, &projects[i].name))
                        .map(|i| (i, Confidence::Inferred))
                        .collect(),
                    Err(m) => {
                        report(m, &mut diags);
                        continue;
                    }
                }
            } else {
                match by_name.get(pattern).map(Vec::as_slice) {
                    Some([one]) => vec![(*one, Confidence::Exact)],
                    Some(_) => {
                        report(
                            format!("implicit dependency {entry} names several projects"),
                            &mut diags,
                        );
                        continue;
                    }
                    None => {
                        report(
                            format!("implicit dependency {entry} names no project"),
                            &mut diags,
                        );
                        continue;
                    }
                }
            };
            if negated {
                removed.extend(matched.into_iter().map(|(i, _)| i));
            } else {
                for (i, c) in matched {
                    let e = targets.entry(i).or_insert(c);
                    *e = (*e).max(c);
                }
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
