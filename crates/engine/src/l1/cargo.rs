//! L1 for Cargo: one system per package, `depends-on` edges between them.
//!
//! `Cargo.toml` files are parsed with `toml`; `cargo` is never run (§5.5).
//!
//! - **Nodes:** every package manifest in the tree, whether or not it belongs
//!   to a workspace. A manifest that cannot be parsed, or that has neither
//!   `[package]` nor `[workspace]`, still becomes a node, with `unknown`
//!   confidence (§5.5). A virtual workspace root is not a node.
//! - **Workspaces**, as Cargo decides them: a package's workspace is the one
//!   it names with `package.workspace`, or else the nearest enclosing
//!   workspace root, unless that root `exclude`s it. It is a member if it is that
//!   root, matches `members` (globs `*`, `?`, `**`), or is a path dependency
//!   of a member under the root. An explicit (non-glob) member is never
//!   excluded. A package that is not a member belongs to no workspace.
//! - **Edges:** from `[dependencies]`, `[build-dependencies]` (also spelled
//!   `build_dependencies`) and their `[target.*]` forms; dev-dependencies are
//!   left out because they do not shape what ships.
//!   - `exact`: resolved by `path`, directly or through `workspace = true`,
//!     or by the workspace root's `[patch.*]` or `[replace]`.
//!   - `inferred`: a registry dependency whose name matches exactly one
//!     package in the same workspace. Git and alternative-registry
//!     dependencies are never matched by name.
//!
//! Globs are matched in polynomial time without recursion, and oversized
//! patterns are ignored with a diagnostic, so a hostile manifest cannot hang
//! or crash the run.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};
use toml::{Table, Value};

use super::path::{Dir, Glob, normalise, repo_path};
use super::{Diagnostic, Read};

const MANIFEST: &str = "Cargo.toml";
const DEP_TABLES: [(&str, DepKind); 3] = [
    ("dependencies", DepKind::Normal),
    ("build-dependencies", DepKind::Build),
    ("build_dependencies", DepKind::Build),
];

struct Package {
    dir: Dir,
    manifest: RepoPath,
    /// `None` when the manifest could not be understood.
    name: Option<String>,
    table: Option<Table>,
}

struct Workspace {
    root: Dir,
    members: Vec<Glob>,
    exclude: Vec<Dir>,
    deps: Table,
    /// Names redirected to a local directory by `[patch.*]` or `[replace]`.
    patches: BTreeMap<String, Dir>,
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Cargo,
        dir: repo_path(dir),
    })
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
    fn from(root: Dir, manifest: &RepoPath, table: &Table, diags: &mut Vec<Diagnostic>) -> Self {
        let empty = Table::new();
        let ws = table
            .get("workspace")
            .and_then(Value::as_table)
            .unwrap_or(&empty);
        let mut members = Vec::new();
        for m in strings(ws.get("members")) {
            match Glob::compile(&m) {
                Ok(Some(g)) => members.push(g),
                Ok(None) => {}
                Err(message) => diags.push(Diagnostic {
                    path: manifest.clone(),
                    message,
                }),
            }
        }
        let exclude = strings(ws.get("exclude"))
            .iter()
            .filter_map(|e| normalise(&root, e))
            .collect();
        let deps = ws
            .get("dependencies")
            .and_then(Value::as_table)
            .cloned()
            .unwrap_or_default();
        let mut patches = BTreeMap::new();
        let patch_tables = table
            .get("patch")
            .and_then(Value::as_table)
            .into_iter()
            .flat_map(|p| p.values().filter_map(Value::as_table));
        for t in patch_tables {
            for (key, spec) in t {
                let name = spec.get("package").and_then(Value::as_str).unwrap_or(key);
                if let Some(dir) = spec
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(|p| normalise(&root, p))
                {
                    patches.insert(name.to_owned(), dir);
                }
            }
        }
        if let Some(replace) = table.get("replace").and_then(Value::as_table) {
            for (key, spec) in replace {
                let name = key.split(':').next().unwrap_or(key);
                if let Some(dir) = spec
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(|p| normalise(&root, p))
                {
                    patches.insert(name.to_owned(), dir);
                }
            }
        }
        Self {
            root,
            members,
            exclude,
            deps,
            patches,
        }
    }

    /// Whether `rel` (relative to the root) is named by a non-glob member.
    fn explicit(&self, rel: &[String]) -> bool {
        self.members.iter().any(|g| g.is_literal(rel))
    }

    /// Whether `dir` is excluded. The root and explicit members never are.
    fn excludes(&self, dir: &[String]) -> bool {
        let Some(rel) = dir.strip_prefix(self.root.as_slice()) else {
            return true;
        };
        !rel.is_empty() && !self.explicit(rel) && self.exclude.iter().any(|e| dir.starts_with(e))
    }

    /// Whether `dir` is a member through being the root or matching `members`.
    fn lists(&self, dir: &[String]) -> bool {
        let Some(rel) = dir.strip_prefix(self.root.as_slice()) else {
            return false;
        };
        rel.is_empty() || self.members.iter().any(|g| g.matches(rel))
    }
}

fn read_manifest(path: &RepoPath, read: Read<'_>) -> Result<Table, String> {
    let bytes = read(path).ok_or("cannot be read")?;
    let text = String::from_utf8(bytes).map_err(|_| "is not UTF-8".to_owned())?;
    text.parse::<Table>()
        .map_err(|e| e.to_string().trim().to_owned())
}

/// The directory a dependency's `path` points to, directly or through
/// `workspace = true`. `Ok(None)` if it has no path.
fn dep_path(
    key: &str,
    spec: &Value,
    pkg: &Package,
    ws: Option<&Workspace>,
) -> Result<Option<Dir>, String> {
    let (base, rel) = match spec.get("path").and_then(Value::as_str) {
        Some(p) => (pkg.dir.as_slice(), p),
        None => match inherited(key, spec, ws)? {
            Some((w, v)) => match v.get("path").and_then(Value::as_str) {
                Some(p) => (w.root.as_slice(), p),
                None => return Ok(None),
            },
            None => return Ok(None),
        },
    };
    normalise(base, rel)
        .map(Some)
        .ok_or_else(|| format!("`{key}` has a path outside the repository: {rel}"))
}

/// For `workspace = true`, the workspace and its entry for `key`.
fn inherited<'w>(
    key: &str,
    spec: &Value,
    ws: Option<&'w Workspace>,
) -> Result<Option<(&'w Workspace, &'w Value)>, String> {
    if spec.get("workspace").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let ws = ws.ok_or_else(|| format!("`{key}` uses workspace = true outside a workspace"))?;
    let v = ws
        .deps
        .get(key)
        .ok_or_else(|| format!("`{key}` is not in [workspace.dependencies]"))?;
    Ok(Some((ws, v)))
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
        let degraded = |message: String, diags: &mut Vec<Diagnostic>| {
            diags.push(Diagnostic {
                path: path.clone(),
                message,
            });
            Package {
                dir: dir.clone(),
                manifest: path.clone(),
                name: None,
                table: None,
            }
        };
        let table = match read_manifest(path, read) {
            Ok(t) => t,
            Err(message) => {
                packages.push(degraded(message, &mut diags));
                continue;
            }
        };
        let is_ws = table.get("workspace").is_some_and(Value::is_table);
        if is_ws {
            workspaces.push(Workspace::from(dir.clone(), path, &table, &mut diags));
        }
        match table.get("package") {
            None if is_ws => {}
            None => packages.push(degraded(
                "neither [package] nor [workspace]".into(),
                &mut diags,
            )),
            Some(pkg) => match pkg.get("name").and_then(Value::as_str).map(str::to_owned) {
                None => packages.push(degraded("[package] has no name".into(), &mut diags)),
                Some(name) => packages.push(Package {
                    dir,
                    manifest: path.clone(),
                    name: Some(name),
                    table: Some(table),
                }),
            },
        }
    }

    let ws_of = membership(&packages, &workspaces);

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
            visibility: None,
            code: None,
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: p.manifest.clone(),
                message: e.to_string(),
            });
        }
    }

    let mut edges: BTreeMap<(usize, usize), (Confidence, BTreeSet<DepKind>)> = BTreeMap::new();
    for (from, p) in packages.iter().enumerate() {
        let Some(table) = &p.table else { continue };
        let ws = ws_of[from].map(|i| &workspaces[i]);
        for (key, spec, kind) in dependency_entries(table) {
            let found = resolve(key, spec, p, ws, &by_dir)
                .map(|r| r.or_else(|| infer(key, spec, ws, from, &by_name, &ws_of)));
            match found {
                Ok(Some((to, conf))) if to != from => {
                    let e = edges.entry((from, to)).or_insert((conf, BTreeSet::new()));
                    e.0 = e.0.max(conf);
                    e.1.insert(kind);
                }
                Ok(_) => {}
                Err(message) => diags.push(Diagnostic {
                    path: p.manifest.clone(),
                    message,
                }),
            }
        }
    }
    for ((from, to), (confidence, dependency)) in edges {
        let edge = Edge {
            key: EdgeKey {
                from: node_id(&packages[from].dir),
                to: node_id(&packages[to].dir),
                kind: EdgeKind::DependsOn,
            },
            confidence,
            evidence: BTreeSet::new(),
            dependency,
        };
        if let Err(e) = model.add_edge(edge) {
            diags.push(Diagnostic {
                path: packages[from].manifest.clone(),
                message: e.to_string(),
            });
        }
    }
    diags.sort();
    diags.dedup();
    diags
}

/// Which workspace each package is a member of, by index.
fn membership(packages: &[Package], workspaces: &[Workspace]) -> Vec<Option<usize>> {
    // The workspace each package would belong to, member or not, and
    // whether the package named it with `package.workspace`.
    let found: Vec<(Option<usize>, bool)> = packages
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
            if let Some(root) = explicit {
                return (workspaces.iter().position(|w| w.root == root), true);
            }
            // Cargo stops at the nearest enclosing root; if that root
            // excludes the package, it has no workspace.
            let nearest = workspaces
                .iter()
                .enumerate()
                .filter(|(_, w)| p.dir.starts_with(&w.root))
                .max_by_key(|(_, w)| w.root.len())
                .filter(|(_, w)| !w.excludes(&p.dir))
                .map(|(i, _)| i);
            (nearest, false)
        })
        .collect();
    let candidate: Vec<Option<usize>> = found.iter().map(|(c, _)| *c).collect();
    // A package that names its root is taken at its word: Cargo rejects the
    // manifest unless the root lists it too.
    let mut member: Vec<bool> = packages
        .iter()
        .zip(&found)
        .map(|(p, (c, named))| c.is_some_and(|w| *named || workspaces[w].lists(&p.dir)))
        .collect();
    let by_dir: BTreeMap<&[String], usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dir.as_slice(), i))
        .collect();
    // Path dependencies of members under the same root are members too.
    loop {
        let mut grew = false;
        for i in 0..packages.len() {
            let (Some(w), true, Some(table)) = (candidate[i], member[i], &packages[i].table) else {
                continue;
            };
            for (key, spec, _) in dependency_entries(table) {
                let Ok(Some(dir)) = dep_path(key, spec, &packages[i], Some(&workspaces[w])) else {
                    continue;
                };
                if let Some(&j) = by_dir.get(dir.as_slice())
                    && !member[j]
                    && candidate[j] == Some(w)
                {
                    member[j] = true;
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    candidate
        .into_iter()
        .zip(member)
        .map(|(c, m)| c.filter(|_| m))
        .collect()
}

/// Every `(name, spec)` in the dependency tables that count for L1.
fn dependency_entries(table: &Table) -> Vec<(&str, &Value, DepKind)> {
    fn tables_in(t: &Table) -> Vec<(&Table, DepKind)> {
        DEP_TABLES
            .iter()
            .filter_map(|(name, kind)| Some((t.get(*name)?.as_table()?, *kind)))
            .collect()
    }
    let mut tables = tables_in(table);
    if let Some(targets) = table.get("target").and_then(Value::as_table) {
        for t in targets.values().filter_map(Value::as_table) {
            tables.extend(tables_in(t));
        }
    }
    tables
        .into_iter()
        .flat_map(|(t, kind)| t.iter().map(move |(k, v)| (k.as_str(), v, kind)))
        .collect()
}

/// The package name a dependency refers to, after `package = ".."` renames.
fn real_name<'a>(key: &'a str, spec: &'a Value, ws: Option<&'a Workspace>) -> &'a str {
    if let Some(n) = spec.get("package").and_then(Value::as_str) {
        return n;
    }
    if let Ok(Some((_, v))) = inherited(key, spec, ws)
        && let Some(n) = v.get("package").and_then(Value::as_str)
    {
        return n;
    }
    key
}

/// Resolves a dependency exactly: by path, or by the workspace's patches.
/// `Ok(None)` if neither applies; `Err` if a path leads nowhere.
fn resolve(
    key: &str,
    spec: &Value,
    pkg: &Package,
    ws: Option<&Workspace>,
    by_dir: &BTreeMap<&[String], usize>,
) -> Result<Option<(usize, Confidence)>, String> {
    if let Some(target) = dep_path(key, spec, pkg, ws)? {
        return match by_dir.get(target.as_slice()) {
            Some(&i) => Ok(Some((i, Confidence::Exact))),
            None => Err(format!(
                "`{key}` points to {}, which has no package",
                repo_path(&target)
            )),
        };
    }
    let patched = ws.and_then(|w| w.patches.get(real_name(key, spec, ws)));
    Ok(patched
        .and_then(|dir| by_dir.get(dir.as_slice()))
        .map(|&i| (i, Confidence::Exact)))
}

/// Infers a registry dependency from a unique same-workspace name match.
fn infer(
    key: &str,
    spec: &Value,
    ws: Option<&Workspace>,
    from: usize,
    by_name: &BTreeMap<&str, Vec<usize>>,
    ws_of: &[Option<usize>],
) -> Option<(usize, Confidence)> {
    let external = |v: &Value| {
        ["git", "registry", "registry-index"]
            .iter()
            .any(|k| v.get(*k).is_some())
    };
    if external(spec)
        || inherited(key, spec, ws)
            .ok()
            .flatten()
            .is_some_and(|(_, v)| external(v))
    {
        return None;
    }
    let here = ws_of.get(from).copied().flatten()?;
    let same: Vec<usize> = by_name
        .get(real_name(key, spec, ws))?
        .iter()
        .copied()
        .filter(|&c| ws_of[c] == Some(here))
        .collect();
    match same.as_slice() {
        [one] => Some((*one, Confidence::Inferred)),
        _ => None,
    }
}
