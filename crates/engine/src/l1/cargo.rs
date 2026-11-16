//! L1 for Cargo: one system per package, `depends-on` edges between them.
//!
//! `Cargo.toml` files are parsed with `toml`; `cargo` is never run (§5.5).
//!
//! - **Nodes:** every package manifest in the tree, whether or not it belongs
//!   to a workspace. A manifest that cannot be parsed still becomes a node,
//!   with `unknown` confidence (§5.5). A virtual workspace root is not a node.
//! - **Workspaces:** a package belongs to the nearest enclosing workspace that
//!   lists it in `members` (globs allowed) and does not `exclude` it, or that
//!   it names with `package.workspace`. The root package belongs to its own.
//! - **Edges:** from `[dependencies]`, `[build-dependencies]` and their
//!   `[target.*]` forms; dev-dependencies are left out because they do not
//!   shape what ships. A dependency resolved by `path`, directly or through
//!   `workspace = true`, is `exact`. One that matches a package only by name
//!   is `inferred`, preferring a package in the same workspace and skipped if
//!   the name is ambiguous.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind, RepoPath,
    SystemKey,
};
use toml::{Table, Value};

use super::{Diagnostic, Read};

const MANIFEST: &str = "Cargo.toml";
const DEP_TABLES: [&str; 2] = ["dependencies", "build-dependencies"];

type Dir = Vec<String>;

struct Package {
    dir: Dir,
    manifest: RepoPath,
    /// `None` when the manifest could not be understood.
    name: Option<String>,
    table: Option<Table>,
}

struct Workspace {
    root: Dir,
    members: Vec<Vec<String>>,
    exclude: Vec<Dir>,
    deps: Table,
}

fn repo_path(dir: &[String]) -> RepoPath {
    // Segments come from parsed RepoPaths or from `normalise`, which yields
    // only valid segments, so this cannot fail; fall back to the root anyway.
    RepoPath::parse(&dir.join("/")).unwrap_or_else(|_| RepoPath::root())
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Cargo,
        dir: repo_path(dir),
    })
}

/// Joins a relative path written in a manifest onto `base`, resolving `.`
/// and `..`. `None` if it is absolute or climbs above the repository root.
fn normalise(base: &[String], rel: &str) -> Option<Dir> {
    let rel = rel.replace('\\', "/");
    if rel.starts_with('/') || rel.contains(':') {
        return None;
    }
    let mut out: Dir = base.to_vec();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s.to_owned()),
        }
    }
    Some(out)
}

/// Matches one path segment against a glob segment with `*` and `?`.
fn segment_matches(pat: &[char], s: &[char]) -> bool {
    match (pat.first(), s.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            segment_matches(&pat[1..], s) || (!s.is_empty() && segment_matches(pat, &s[1..]))
        }
        (Some('?'), Some(_)) => segment_matches(&pat[1..], &s[1..]),
        (Some(p), Some(c)) if p == c => segment_matches(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// Matches a relative directory against a glob of segments; `**` matches
/// any number of segments.
fn glob_matches(pat: &[String], dir: &[String]) -> bool {
    match pat.first() {
        None => dir.is_empty(),
        Some(p) if p == "**" => (0..=dir.len()).any(|i| glob_matches(&pat[1..], &dir[i..])),
        Some(p) => {
            let (pc, dc): (Vec<char>, Vec<char>) = match dir.first() {
                Some(d) => (p.chars().collect(), d.chars().collect()),
                None => return false,
            };
            segment_matches(&pc, &dc) && glob_matches(&pat[1..], &dir[1..])
        }
    }
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

impl Workspace {
    fn from(root: Dir, ws: &Table) -> Self {
        let members = strings(ws.get("members"))
            .iter()
            .filter_map(|m| normalise(&[], m))
            .collect();
        let exclude = strings(ws.get("exclude"))
            .iter()
            .filter_map(|e| normalise(&root, e))
            .collect();
        let deps = ws
            .get("dependencies")
            .and_then(Value::as_table)
            .cloned()
            .unwrap_or_default();
        Self {
            root,
            members,
            exclude,
            deps,
        }
    }

    fn lists(&self, dir: &[String]) -> bool {
        if !dir.starts_with(&self.root) || self.exclude.iter().any(|e| dir.starts_with(e)) {
            return false;
        }
        let rel = &dir[self.root.len()..];
        rel.is_empty() || self.members.iter().any(|m| glob_matches(m, rel))
    }
}

fn read_manifest(path: &RepoPath, read: Read<'_>) -> Result<Table, String> {
    let bytes = read(path).ok_or("cannot be read")?;
    let text = String::from_utf8(bytes).map_err(|_| "is not UTF-8".to_owned())?;
    text.parse::<Table>()
        .map_err(|e| e.to_string().trim().to_owned())
}

/// Adds the Cargo systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut packages: Vec<Package> = Vec::new();
    let mut workspaces: Vec<Workspace> = Vec::new();

    for path in paths
        .iter()
        .filter(|p| p.segments().last().is_some_and(|s| s == MANIFEST))
    {
        let segs = path.segments();
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        match read_manifest(path, read) {
            Err(message) => {
                diags.push(Diagnostic {
                    path: path.clone(),
                    message,
                });
                packages.push(Package {
                    dir,
                    manifest: path.clone(),
                    name: None,
                    table: None,
                });
            }
            Ok(table) => {
                if let Some(ws) = table.get("workspace").and_then(Value::as_table) {
                    workspaces.push(Workspace::from(dir.clone(), ws));
                }
                match table.get("package") {
                    None => {}
                    Some(pkg) => {
                        let name = pkg.get("name").and_then(Value::as_str).map(str::to_owned);
                        if name.is_none() {
                            diags.push(Diagnostic {
                                path: path.clone(),
                                message: "[package] has no name".into(),
                            });
                        }
                        let table = name.as_ref().map(|_| table.clone());
                        packages.push(Package {
                            dir,
                            manifest: path.clone(),
                            name,
                            table,
                        });
                    }
                }
            }
        }
    }

    // Which workspace each package belongs to, by index.
    let ws_of: Vec<Option<usize>> = packages
        .iter()
        .map(|p| {
            let explicit = p
                .table
                .as_ref()
                .and_then(|t| {
                    t.get("package")?
                        .get("workspace")?
                        .as_str()
                        .map(str::to_owned)
                })
                .and_then(|w| normalise(&p.dir, &w));
            workspaces
                .iter()
                .enumerate()
                .filter(|(_, w)| match &explicit {
                    Some(root) => w.root == *root,
                    None => w.lists(&p.dir),
                })
                .max_by_key(|(_, w)| w.root.len())
                .map(|(i, _)| i)
        })
        .collect();

    let by_dir: BTreeMap<&[String], usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dir.as_slice(), i))
        .collect();
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, p) in packages.iter().enumerate() {
        if let Some(n) = &p.name {
            by_name.entry(n.as_str()).or_default().push(i);
        }
    }

    for p in &packages {
        let fallback = p.dir.last().cloned().unwrap_or_else(|| "(root)".to_owned());
        let node = Node {
            id: node_id(&p.dir),
            kind: NodeKind::System,
            name: p.name.clone().unwrap_or(fallback),
            source: p.manifest.clone(),
            parent: None,
            confidence: if p.name.is_some() {
                Confidence::Exact
            } else {
                Confidence::Unknown
            },
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: p.manifest.clone(),
                message: e.to_string(),
            });
        }
    }

    let mut edges: BTreeMap<(usize, usize), Confidence> = BTreeMap::new();
    for (from, p) in packages.iter().enumerate() {
        let Some(table) = &p.table else { continue };
        let ws = ws_of[from].map(|i| &workspaces[i]);
        for (key, spec) in dependency_entries(table) {
            match resolve(key, spec, from, p, ws, &by_dir, &by_name, &ws_of) {
                Ok(Some((to, conf))) if to != from => {
                    let e = edges.entry((from, to)).or_insert(conf);
                    *e = (*e).max(conf);
                }
                Ok(_) => {}
                Err(message) => diags.push(Diagnostic {
                    path: p.manifest.clone(),
                    message,
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
        };
        if let Err(e) = model.add_edge(edge) {
            diags.push(Diagnostic {
                path: packages[from].manifest.clone(),
                message: e.to_string(),
            });
        }
    }
    diags.sort();
    diags
}

/// Every `(name, spec)` in the dependency tables that count for L1.
fn dependency_entries(table: &Table) -> Vec<(&str, &Value)> {
    let mut tables: Vec<&Table> = DEP_TABLES
        .iter()
        .filter_map(|t| table.get(*t)?.as_table())
        .collect();
    if let Some(targets) = table.get("target").and_then(Value::as_table) {
        for t in targets.values().filter_map(Value::as_table) {
            tables.extend(DEP_TABLES.iter().filter_map(|d| t.get(*d)?.as_table()));
        }
    }
    tables
        .into_iter()
        .flat_map(|t| t.iter().map(|(k, v)| (k.as_str(), v)))
        .collect()
}

/// Resolves one dependency to a package index and a confidence. `Ok(None)`
/// means an external dependency; `Err` a dependency that should have
/// resolved and did not.
#[allow(clippy::too_many_arguments)]
fn resolve(
    key: &str,
    spec: &Value,
    from: usize,
    pkg: &Package,
    ws: Option<&Workspace>,
    by_dir: &BTreeMap<&[String], usize>,
    by_name: &BTreeMap<&str, Vec<usize>>,
    ws_of: &[Option<usize>],
) -> Result<Option<(usize, Confidence)>, String> {
    let mut name = spec.get("package").and_then(Value::as_str).unwrap_or(key);
    let mut path = spec
        .get("path")
        .and_then(Value::as_str)
        .map(|p| (pkg.dir.as_slice(), p));
    if spec.get("workspace").and_then(Value::as_bool) == Some(true) {
        let ws = ws.ok_or_else(|| format!("`{key}` uses workspace = true outside a workspace"))?;
        let inherited = ws
            .deps
            .get(key)
            .ok_or_else(|| format!("`{key}` is not in [workspace.dependencies]"))?;
        if let Some(n) = inherited.get("package").and_then(Value::as_str) {
            name = n;
        }
        if let Some(p) = inherited.get("path").and_then(Value::as_str) {
            path = Some((ws.root.as_slice(), p));
        }
    }
    if let Some((base, rel)) = path {
        let target = normalise(base, rel)
            .ok_or_else(|| format!("`{key}` has a path outside the repository: {rel}"))?;
        return match by_dir.get(target.as_slice()) {
            Some(&i) => Ok(Some((i, Confidence::Exact))),
            None => Err(format!(
                "`{key}` points to {}, which has no package",
                repo_path(&target)
            )),
        };
    }
    let Some(candidates) = by_name.get(name) else {
        return Ok(None);
    };
    let here = ws_of.get(from).copied().flatten();
    let same_ws: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|&c| here.is_some() && ws_of[c] == here)
        .collect();
    let pick = match (same_ws.as_slice(), candidates.as_slice()) {
        ([one], _) | ([], [one]) => Some(*one),
        _ => None,
    };
    Ok(pick.map(|i| (i, Confidence::Inferred)))
}
