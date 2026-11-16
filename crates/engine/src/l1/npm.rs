//! L1 for npm, yarn and pnpm: one system per `package.json`.
//!
//! Manifests are parsed as JSON (and `pnpm-workspace.yaml` by a minimal
//! line reader); no package manager is ever run (§5.5). Files under
//! `node_modules` are ignored.
//!
//! - **Nodes:** every `package.json` that is not a workspace root, named by
//!   its `name` (or its directory). Malformed JSON still becomes a node,
//!   with `unknown` confidence.
//! - **Workspaces:** a `package.json` with `workspaces` (an array, or an
//!   object with `packages`), or a directory with `pnpm-workspace.yaml`, is
//!   a root. Its patterns are directory globs; a leading `!` excludes. A
//!   package belongs to the nearest enclosing root that lists it.
//! - **Edges:** from `dependencies`, `optionalDependencies` and
//!   `peerDependencies` (`devDependencies` do not ship).
//!   - `exact`: `workspace:` references within the workspace, and `file:`,
//!     `link:` or `portal:` paths to a package.
//!   - `inferred`: a version range whose name matches exactly one package
//!     in the same workspace (npm links it if the version fits).
//!   - Git, URL and `npm:` alias specs are external and never matched.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};
use serde_json::Value;

use super::path::{Dir, Glob, normalise, repo_path};
use super::{Diagnostic, Read};

const MANIFEST: &str = "package.json";
const PNPM_WORKSPACE: &str = "pnpm-workspace.yaml";
const DEP_FIELDS: [&str; 3] = ["dependencies", "optionalDependencies", "peerDependencies"];

struct Package {
    dir: Dir,
    manifest: RepoPath,
    name: Option<String>,
    json: Option<Value>,
}

struct Workspace {
    root: Dir,
    include: Vec<Glob>,
    exclude: Vec<Glob>,
}

impl Workspace {
    fn new(
        root: Dir,
        patterns: Vec<String>,
        source: &RepoPath,
        diags: &mut Vec<Diagnostic>,
    ) -> Self {
        let (mut include, mut exclude) = (Vec::new(), Vec::new());
        for p in patterns {
            let (list, pat) = match p.strip_prefix('!') {
                Some(rest) => (&mut exclude, rest.to_owned()),
                None => (&mut include, p),
            };
            match Glob::compile(&pat) {
                Ok(Some(g)) => list.push(g),
                Ok(None) => {}
                Err(message) => diags.push(Diagnostic {
                    path: source.clone(),
                    message,
                }),
            }
        }
        Self {
            root,
            include,
            exclude,
        }
    }

    fn lists(&self, dir: &[String]) -> bool {
        let Some(rel) = dir.strip_prefix(self.root.as_slice()) else {
            return false;
        };
        !rel.is_empty()
            && self.include.iter().any(|g| g.matches(rel))
            && !self.exclude.iter().any(|g| g.matches(rel))
    }
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Npm,
        dir: repo_path(dir),
    })
}

fn in_node_modules(p: &RepoPath) -> bool {
    p.segments().iter().any(|s| s == "node_modules")
}

/// The `packages:` list of a `pnpm-workspace.yaml`, read line by line: the
/// file is YAML, but this one key is always a flat list of strings.
fn pnpm_packages(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t', '-']) {
            inside = trimmed.starts_with("packages:");
            continue;
        }
        if inside && let Some(item) = trimmed.strip_prefix('-') {
            let item = item.split(" #").next().unwrap_or("").trim();
            let item = item.trim_matches(|c| c == '"' || c == '\'');
            if !item.is_empty() {
                out.push(item.to_owned());
            }
        }
    }
    out
}

fn workspace_patterns(json: &Value) -> Option<Vec<String>> {
    let ws = json.get("workspaces")?;
    let list = ws.as_array().or_else(|| ws.get("packages")?.as_array())?;
    Some(
        list.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    )
}

/// Adds the npm systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut packages: Vec<Package> = Vec::new();
    let mut workspaces: Vec<Workspace> = Vec::new();

    for path in paths.iter().filter(|p| !in_node_modules(p)) {
        let segs = path.segments();
        let Some(file) = segs.last() else { continue };
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        if file == PNPM_WORKSPACE {
            let text = read(path)
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
            workspaces.push(Workspace::new(dir, pnpm_packages(&text), path, &mut diags));
            continue;
        }
        if file != MANIFEST {
            continue;
        }
        let parsed = read(path)
            .ok_or_else(|| "cannot be read".to_owned())
            .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()))
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
                packages.push(Package {
                    dir,
                    manifest: path.clone(),
                    name: None,
                    json: None,
                });
            }
            Ok(json) => {
                if let Some(patterns) = workspace_patterns(&json) {
                    workspaces.push(Workspace::new(dir, patterns, path, &mut diags));
                    continue;
                }
                let name = json.get("name").and_then(Value::as_str).map(str::to_owned);
                packages.push(Package {
                    dir,
                    manifest: path.clone(),
                    name,
                    json: Some(json),
                });
            }
        }
    }
    // A pnpm-workspace.yaml next to a package.json makes that directory a
    // root: drop it from the packages.
    let roots: BTreeSet<&[String]> = workspaces.iter().map(|w| w.root.as_slice()).collect();
    packages.retain(|p| !roots.contains(p.dir.as_slice()));

    // Each package's workspace: the nearest enclosing root that lists it.
    let ws_of: Vec<Option<usize>> = packages
        .iter()
        .map(|p| {
            workspaces
                .iter()
                .enumerate()
                .filter(|(_, w)| p.dir.starts_with(&w.root) && p.dir != w.root)
                .max_by_key(|(_, w)| w.root.len())
                .filter(|(_, w)| w.lists(&p.dir))
                .map(|(i, _)| i)
        })
        .collect();
    let by_dir: BTreeMap<&[String], usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dir.as_slice(), i))
        .collect();

    for p in &packages {
        let fallback = p.dir.last().cloned().unwrap_or_else(|| "(root)".to_owned());
        let node = Node {
            id: node_id(&p.dir),
            kind: NodeKind::System,
            name: p.name.clone().unwrap_or(fallback),
            source: p.manifest.clone(),
            parent: None,
            confidence: if p.json.is_some() {
                Confidence::Exact
            } else {
                Confidence::Unknown
            },
            visibility: None,
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
        let Some(json) = &p.json else { continue };
        for field in DEP_FIELDS {
            let Some(deps) = json.get(field).and_then(Value::as_object) else {
                continue;
            };
            for (name, spec) in deps {
                let spec = spec.as_str().unwrap_or("");
                match resolve(name, spec, from, p, &packages, &ws_of, &by_dir) {
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
                path: packages[from].manifest.clone(),
                message: e.to_string(),
            });
        }
    }
    diags.sort();
    diags.dedup();
    diags
}

/// Resolves one dependency. `Ok(None)` for an external dependency; `Err` for
/// a local reference that leads nowhere.
fn resolve(
    name: &str,
    spec: &str,
    from: usize,
    pkg: &Package,
    packages: &[Package],
    ws_of: &[Option<usize>],
    by_dir: &BTreeMap<&[String], usize>,
) -> Result<Option<(usize, Confidence)>, String> {
    let same_ws = |n: &str| -> Vec<usize> {
        let Some(here) = ws_of[from] else {
            return Vec::new();
        };
        (0..packages.len())
            .filter(|&i| ws_of[i] == Some(here) && packages[i].name.as_deref() == Some(n))
            .collect()
    };
    if let Some(rest) = spec.strip_prefix("workspace:") {
        // `workspace:*`, `workspace:^1`, or `workspace:other-name@*`.
        let target = match rest.rsplit_once('@') {
            Some((alias, _)) if !alias.is_empty() => alias,
            _ => name,
        };
        return match same_ws(target).as_slice() {
            [one] => Ok(Some((*one, Confidence::Exact))),
            _ => Err(format!(
                "`{name}` uses workspace: but no single workspace package is named `{target}`"
            )),
        };
    }
    for proto in ["file:", "link:", "portal:"] {
        if let Some(rel) = spec.strip_prefix(proto) {
            let target = normalise(&pkg.dir, rel)
                .ok_or_else(|| format!("`{name}` has a path outside the repository: {rel}"))?;
            return match by_dir.get(target.as_slice()) {
                Some(&i) => Ok(Some((i, Confidence::Exact))),
                // A file: path may name a tarball or a folder without a
                // package.json: external, not an error.
                None => Ok(None),
            };
        }
    }
    // `npm:` aliases, `github:` and other protocols, URLs and `user/repo`
    // shorthands all come from outside the repository.
    let external = spec.contains(':') || (spec.contains('/') && !spec.starts_with('.'));
    if external {
        return Ok(None);
    }
    Ok(match same_ws(name).as_slice() {
        [one] => Some((*one, Confidence::Inferred)),
        _ => None,
    })
}
