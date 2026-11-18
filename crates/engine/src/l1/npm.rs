//! L1 for npm, yarn and pnpm: one system per `package.json`.
//!
//! Manifests are parsed as JSON (and `pnpm-workspace.yaml` by a minimal
//! reader for its `packages` list); no package manager is ever run (§5.5).
//! Files under `node_modules` are ignored, and a UTF-8 byte-order mark is
//! skipped as npm does.
//!
//! - **Nodes:** every `package.json`, named by its `name` (or directory),
//!   except a workspace root that has no `name`. Malformed JSON still
//!   becomes a node, with `unknown` confidence.
//! - **Workspaces:** a `package.json` with `workspaces` (an array, or an
//!   object with `packages`), or a `pnpm-workspace.yaml` with a `packages`
//!   key, makes its directory a root. Patterns are directory globs with
//!   `*`, `**`, `?`, `[..]` and `{a,b}`; a leading `!` excludes. A package
//!   belongs to the nearest enclosing root that lists it; a root package
//!   that no outer root lists belongs to its own workspace.
//! - **Edges:** from `dependencies`, `optionalDependencies` and
//!   `peerDependencies` (`devDependencies` do not ship).
//!   - `exact`: `workspace:` references within the workspace, and `file:`,
//!     `link:` or `portal:` paths (or bare `./`, `../` paths) to a package.
//!   - `inferred`: in an npm or yarn workspace, a version range whose name
//!     matches exactly one package of the workspace. pnpm only links
//!     workspace packages through `workspace:` (since pnpm 9), so no name
//!     is inferred there.
//!   - Git, URL, `npm:` alias, `catalog:`, tarball and `user/repo` specs
//!     are external and never matched.

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
    /// pnpm links workspace packages only through `workspace:`.
    pnpm: bool,
}

impl Workspace {
    fn new(
        root: Dir,
        patterns: Vec<String>,
        pnpm: bool,
        source: &RepoPath,
        diags: &mut Vec<Diagnostic>,
    ) -> Self {
        let (mut include, mut exclude) = (Vec::new(), Vec::new());
        for p in patterns {
            let (list, pat) = match p.strip_prefix('!') {
                Some(rest) => (&mut exclude, rest.to_owned()),
                None => (&mut include, p),
            };
            match Glob::compile_all(&pat) {
                Ok(globs) => list.extend(globs),
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
            pnpm,
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

fn text_of(bytes: Vec<u8>) -> Option<String> {
    let s = String::from_utf8(bytes).ok()?;
    Some(s.strip_prefix('\u{feff}').map(str::to_owned).unwrap_or(s))
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '"' || c == '\'').to_owned()
}

/// The `packages` list of a `pnpm-workspace.yaml`, in block form
/// (`- 'a/*'` lines) or flow form (`[a, 'b']`). `None` if there is no
/// `packages` key: the file then only holds settings.
fn pnpm_packages(text: &str) -> Option<Vec<String>> {
    let mut out = None;
    let mut inside = false;
    for line in text.lines() {
        let code = line.split(" #").next().unwrap_or("");
        let trimmed = code.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t', '-']) {
            inside = false;
            if let Some(rest) = trimmed.strip_prefix("packages:") {
                let list: &mut Vec<String> = out.get_or_insert_with(Vec::new);
                let rest = rest.trim();
                if let Some(flow) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                    list.extend(flow.split(',').map(unquote).filter(|s| !s.is_empty()));
                } else {
                    inside = true;
                }
            }
            continue;
        }
        if inside && let Some(item) = trimmed.strip_prefix('-') {
            let item = unquote(item);
            if !item.is_empty() {
                out.get_or_insert_with(Vec::new).push(item);
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
            match read(path).and_then(text_of) {
                None => diags.push(Diagnostic {
                    path: path.clone(),
                    message: "cannot be read as UTF-8".into(),
                }),
                Some(text) => {
                    if let Some(list) = pnpm_packages(&text) {
                        workspaces.push(Workspace::new(dir, list, true, path, &mut diags));
                    }
                }
            }
            continue;
        }
        if file != MANIFEST {
            continue;
        }
        let parsed = read(path)
            .and_then(text_of)
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
                packages.push(Package {
                    dir,
                    manifest: path.clone(),
                    name: None,
                    json: None,
                });
            }
            Ok(json) => {
                let name = json.get("name").and_then(Value::as_str).map(str::to_owned);
                if let Some(patterns) = workspace_patterns(&json) {
                    workspaces.push(Workspace::new(
                        dir.clone(),
                        patterns,
                        false,
                        path,
                        &mut diags,
                    ));
                    if name.is_none() {
                        continue;
                    }
                }
                packages.push(Package {
                    dir,
                    manifest: path.clone(),
                    name,
                    json: Some(json),
                });
            }
        }
    }
    // Two root files in one directory (package.json workspaces and a pnpm
    // yaml) make one workspace; pnpm's rules win.
    workspaces.sort_by(|a, b| a.root.cmp(&b.root).then(b.pnpm.cmp(&a.pnpm)));
    workspaces.dedup_by(|b, a| a.root == b.root);
    // A nameless package.json in a pnpm root directory is not a system.
    let roots: BTreeSet<&[String]> = workspaces.iter().map(|w| w.root.as_slice()).collect();
    packages.retain(|p| p.name.is_some() || p.json.is_none() || !roots.contains(p.dir.as_slice()));

    // Each package's workspace: the nearest enclosing root that lists it,
    // else the workspace it is the root of.
    let ws_of: Vec<Option<usize>> = packages
        .iter()
        .map(|p| {
            let listed = workspaces
                .iter()
                .enumerate()
                .filter(|(_, w)| w.lists(&p.dir))
                .max_by_key(|(_, w)| w.root.len())
                .map(|(i, _)| i);
            listed.or_else(|| workspaces.iter().position(|w| w.root == p.dir))
        })
        .collect();
    let by_dir: BTreeMap<&[String], usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dir.as_slice(), i))
        .collect();
    let mut by_ws_name: BTreeMap<(usize, &str), Vec<usize>> = BTreeMap::new();
    for (i, p) in packages.iter().enumerate() {
        if let (Some(w), Some(n)) = (ws_of[i], &p.name) {
            by_ws_name.entry((w, n.as_str())).or_default().push(i);
        }
    }
    // A root package is also part of the workspace it roots, even when an
    // outer workspace lists it (pnpm and yarn include the root project).
    for (w, ws) in workspaces.iter().enumerate() {
        if let Some(&i) = by_dir.get(ws.root.as_slice())
            && ws_of[i] != Some(w)
            && let Some(n) = &packages[i].name
        {
            by_ws_name.entry((w, n.as_str())).or_default().push(i);
        }
    }
    let ctx = Ctx {
        packages: &packages,
        workspaces: &workspaces,
        ws_of: &ws_of,
        by_dir: &by_dir,
        by_ws_name: &by_ws_name,
    };

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
            code: None,
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
                match ctx.resolve(name, spec.as_str().unwrap_or(""), from) {
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

struct Ctx<'a> {
    packages: &'a [Package],
    workspaces: &'a [Workspace],
    ws_of: &'a [Option<usize>],
    by_dir: &'a BTreeMap<&'a [String], usize>,
    by_ws_name: &'a BTreeMap<(usize, &'a str), Vec<usize>>,
}

impl Ctx<'_> {
    /// The single package named `n` in `from`'s workspace, if there is one.
    fn same_ws(&self, from: usize, n: &str) -> Option<usize> {
        let w = self.ws_of[from]?;
        match self.by_ws_name.get(&(w, n))?.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// Resolves one dependency. `Ok(None)` for an external dependency;
    /// `Err` for a local reference that leads nowhere.
    fn resolve(
        &self,
        name: &str,
        spec: &str,
        from: usize,
    ) -> Result<Option<(usize, Confidence)>, String> {
        if let Some(rest) = spec.strip_prefix("workspace:") {
            // `workspace:*`, `workspace:^1`, `workspace:../path`, or
            // `workspace:other-name@*`.
            if rest.starts_with('.') {
                return self.path_dep(name, rest, from);
            }
            let target = match rest.rsplit_once('@') {
                Some((alias, _)) if !alias.is_empty() => alias,
                _ => name,
            };
            return match self.same_ws(from, target) {
                Some(i) => Ok(Some((i, Confidence::Exact))),
                None => Err(format!(
                    "`{name}` uses workspace: but no single workspace package is named `{target}`"
                )),
            };
        }
        for proto in ["file:", "link:", "portal:"] {
            if let Some(rel) = spec.strip_prefix(proto) {
                return self.path_dep(name, rel, from);
            }
        }
        if spec.starts_with("./") || spec.starts_with("../") {
            return self.path_dep(name, spec, from);
        }
        // `npm:` aliases, `catalog:`, `github:` and other protocols, URLs,
        // absolute or home paths, tarballs and `user/repo` shorthands all
        // come from outside the repository.
        let external = spec.contains(':')
            || spec.contains('/')
            || spec.starts_with('~')
            || spec.ends_with(".tgz")
            || spec.ends_with(".tar.gz");
        let pnpm = self.ws_of[from].is_some_and(|w| self.workspaces[w].pnpm);
        if external || pnpm {
            return Ok(None);
        }
        Ok(self.same_ws(from, name).map(|i| (i, Confidence::Inferred)))
    }

    fn path_dep(
        &self,
        name: &str,
        rel: &str,
        from: usize,
    ) -> Result<Option<(usize, Confidence)>, String> {
        if rel.ends_with(".tgz") || rel.ends_with(".tar.gz") {
            return Ok(None);
        }
        let target = normalise(&self.packages[from].dir, rel)
            .ok_or_else(|| format!("`{name}` has a path outside the repository: {rel}"))?;
        // A path to a folder without a package.json is not a system: external.
        Ok(self
            .by_dir
            .get(target.as_slice())
            .map(|&i| (i, Confidence::Exact)))
    }
}
