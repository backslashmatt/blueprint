//! L1 for Python: one system per project directory (#154, DR 0014).
//!
//! `pyproject.toml` is parsed as TOML and `setup.cfg` read for its
//! `[metadata] name`; `setup.py` is never run or read (§5.5). Files under a
//! hidden directory (`.venv`, `.tox`), `venv`, `site-packages`,
//! `__pycache__` or `node_modules` are ignored.
//!
//! - **Nodes:** a directory is a system if its `pyproject.toml` declares a
//!   project (`[project]`, `[tool.poetry]` or `[build-system]`), or it holds
//!   a `setup.cfg` or `setup.py`. A `pyproject.toml` with only tool settings
//!   (`[tool.ruff]`, a uv workspace root's `[tool.uv.workspace]`) is not
//!   one. The name is `[project].name`, else `[tool.poetry].name`, else
//!   `setup.cfg`'s, else the directory's. Confidence: `exact` from a parsed
//!   `pyproject.toml`; `inferred` from `setup.cfg` or `setup.py` alone,
//!   whose real metadata may be computed when it runs; `unknown` when the
//!   `pyproject.toml` cannot be parsed.
//! - **Edges:** from `[project].dependencies` and Poetry's
//!   `[tool.poetry.dependencies]` (optional, dev and group dependencies do
//!   not ship). Names compare after PEP 503 normalisation.
//!   - `exact`: a uv source (`[tool.uv.sources]`) or Poetry dependency
//!     with a `path` to a system, or a uv source with `workspace = true`
//!     naming exactly one system.
//!   - `inferred`: a name that matches exactly one system in the repository.
//!   - Git, URL and index sources, and `name @ url` direct references, are
//!     external and never matched.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};
use toml::{Table, Value};

use super::path::{Dir, normalise, repo_path};
use super::{Diagnostic, Read};

const PYPROJECT: &str = "pyproject.toml";
const SETUP_CFG: &str = "setup.cfg";
const SETUP_PY: &str = "setup.py";

/// Directories whose Python files are not the repository's own.
pub(crate) fn ignored_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "venv" | "site-packages" | "__pycache__" | "node_modules"
        )
}

/// Whether a file sits under an ignored directory.
pub(crate) fn ignored(path: &RepoPath) -> bool {
    let segs = path.segments();
    segs[..segs.len().saturating_sub(1)]
        .iter()
        .any(|s| ignored_dir(s))
}

/// What a project directory declares.
#[derive(Default)]
struct Project {
    dir: Dir,
    /// The file the node points at: `pyproject.toml` if there is one.
    manifest: Option<RepoPath>,
    /// The parsed `pyproject.toml`; `Some(None)` if it did not parse.
    pyproject: Option<Option<Table>>,
    /// `setup.cfg`'s `[metadata] name`.
    cfg_name: Option<String>,
    setup: bool,
}

impl Project {
    fn table(&self) -> Option<&Table> {
        self.pyproject.as_ref()?.as_ref()
    }

    /// Whether the `pyproject.toml` declares a project, rather than only
    /// configuring tools. One that did not parse may: it counts.
    fn declares(&self) -> bool {
        match &self.pyproject {
            Some(None) => true,
            Some(Some(t)) => {
                t.contains_key("project")
                    || t.contains_key("build-system")
                    || poetry(t).is_some_and(|p| p.contains_key("name"))
            }
            None => false,
        }
    }

    /// Whether the directory is a project at all.
    fn is_system(&self) -> bool {
        self.declares() || self.setup
    }

    fn name(&self) -> Option<String> {
        let t = self.table();
        let given = |n: &&str| !n.trim().is_empty();
        t.and_then(|t| t.get("project")?.get("name")?.as_str())
            .filter(given)
            .or_else(|| {
                t.and_then(|t| poetry(t)?.get("name")?.as_str())
                    .filter(given)
            })
            .map(str::to_owned)
            .or_else(|| self.cfg_name.clone())
    }

    /// `exact` from a `pyproject.toml` that declares the project; from
    /// `setup.cfg` or `setup.py` alone (a tool-only `pyproject.toml` beside
    /// them declares nothing), `inferred`.
    fn confidence(&self) -> Confidence {
        match &self.pyproject {
            Some(None) => Confidence::Unknown,
            Some(Some(_)) if self.declares() => Confidence::Exact,
            _ => Confidence::Inferred,
        }
    }
}

fn poetry(t: &Table) -> Option<&Table> {
    t.get("tool")?.get("poetry")?.as_table()
}

/// PEP 503: lower case, every run of `-`, `_` and `.` one `-`.
pub(crate) fn normalise_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut sep = false;
    for c in name.trim().chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
        } else {
            if sep && !out.is_empty() {
                out.push('-');
            }
            sep = false;
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// The distribution a PEP 508 requirement names, unless it is a direct
/// reference (`name @ url`), which comes from outside.
fn requirement_name(req: &str) -> Option<String> {
    let req = req.trim();
    let end = req
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(req.len());
    let name = &req[..end];
    let rest = req[end..].trim_start();
    // `name[extra] @ url`: skip the extras before looking for `@`.
    let rest = match rest.strip_prefix('[') {
        Some(r) => r
            .split_once(']')
            .map_or("", |(_, after)| after)
            .trim_start(),
        None => rest,
    };
    let name = normalise_name(name);
    (!name.is_empty() && !rest.starts_with('@')).then_some(name)
}

/// `setup.cfg`'s `[metadata] name`.
fn setup_cfg_name(text: &str) -> Option<String> {
    let mut in_metadata = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_metadata = t == "[metadata]";
            continue;
        }
        // Continuation lines are indented; the name is a plain key.
        if in_metadata
            && !line.starts_with([' ', '\t'])
            && let Some((k, v)) = t.split_once(['=', ':'])
            && k.trim() == "name"
        {
            let v = v.trim();
            // `name = attr: pkg.NAME` or other computed values are not names.
            if !v.is_empty() && !v.contains(':') && !v.contains(' ') {
                return Some(v.to_owned());
            }
        }
    }
    None
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Python,
        dir: repo_path(dir),
    })
}

/// A dependency as a project declares it.
struct Dependency {
    /// Normalised.
    name: String,
    /// Where uv or Poetry says it comes from, if they say.
    source: Option<Source>,
}

enum Source {
    Path(String),
    Workspace,
    Elsewhere,
}

/// A uv source or Poetry dependency table's source.
fn source_of(v: &Value) -> Option<Source> {
    // uv allows a list of sources with markers; the first decides.
    let v = match v {
        Value::Array(a) => a.first()?,
        v => v,
    };
    let t = v.as_table()?;
    if let Some(p) = t.get("path").and_then(Value::as_str) {
        return Some(Source::Path(p.to_owned()));
    }
    if t.get("workspace").and_then(Value::as_bool) == Some(true) {
        return Some(Source::Workspace);
    }
    ["git", "url", "index", "file"]
        .iter()
        .any(|k| t.contains_key(*k))
        .then_some(Source::Elsewhere)
}

/// The shipped dependencies a `pyproject.toml` declares.
fn dependencies(t: &Table) -> Vec<Dependency> {
    let uv_sources: BTreeMap<String, &Value> = t
        .get("tool")
        .and_then(|t| t.get("uv"))
        .and_then(|u| u.get("sources"))
        .and_then(Value::as_table)
        .map(|s| s.iter().map(|(k, v)| (normalise_name(k), v)).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    let reqs = t
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(Value::as_array);
    for r in reqs.into_iter().flatten().filter_map(Value::as_str) {
        if let Some(name) = requirement_name(r) {
            let source = uv_sources.get(&name).and_then(|v| source_of(v));
            out.push(Dependency { name, source });
        } else if let Some(raw) = r
            .split(['@', '[', ' ', ';'])
            .next()
            .filter(|n| !n.is_empty())
        {
            // A direct reference: external, unless uv sources it locally.
            let name = normalise_name(raw);
            if let Some(source) = uv_sources.get(&name).and_then(|v| source_of(v)) {
                out.push(Dependency {
                    name,
                    source: Some(source),
                });
            }
        }
    }
    let poetry_deps = poetry(t)
        .and_then(|p| p.get("dependencies"))
        .and_then(Value::as_table);
    for (k, v) in poetry_deps.into_iter().flatten() {
        if k.eq_ignore_ascii_case("python") {
            continue;
        }
        // `optional = true`: an extra, which does not ship by default.
        let first = match v {
            Value::Array(a) => a.first(),
            v => Some(v),
        };
        if first
            .and_then(|f| f.get("optional"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            continue;
        }
        let source = source_of(v);
        out.push(Dependency {
            name: normalise_name(k),
            source,
        });
    }
    out
}

/// Adds a system for every Python project in `paths`, and the dependency
/// edges between them.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut projects: BTreeMap<Dir, Project> = BTreeMap::new();
    for path in paths {
        let segs = path.segments();
        let Some(file) = segs.last() else { continue };
        if !matches!(file.as_str(), PYPROJECT | SETUP_CFG | SETUP_PY) || ignored(path) {
            continue;
        }
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        let p = projects.entry(dir.clone()).or_insert_with(|| Project {
            dir,
            ..Project::default()
        });
        match file.as_str() {
            PYPROJECT => {
                let parsed = read(path)
                    .and_then(|b| String::from_utf8(b).ok())
                    .ok_or_else(|| "cannot be read as UTF-8".to_owned())
                    .and_then(|t| {
                        let t = t.strip_prefix('\u{feff}').unwrap_or(&t).to_owned();
                        t.parse::<Table>().map_err(|e| e.to_string())
                    });
                match parsed {
                    Ok(t) => p.pyproject = Some(Some(t)),
                    Err(message) => {
                        diags.push(Diagnostic {
                            path: path.clone(),
                            message,
                        });
                        p.pyproject = Some(None);
                    }
                }
            }
            SETUP_CFG => {
                p.setup = true;
                p.cfg_name = read(path)
                    .and_then(|b| String::from_utf8(b).ok())
                    .and_then(|t| setup_cfg_name(&t));
                // `setup.cfg` is a better source than `setup.py`.
                p.manifest = Some(path.clone());
            }
            _ => {
                p.setup = true;
                if p.manifest.is_none() {
                    p.manifest = Some(path.clone());
                }
            }
        }
    }
    // `pyproject.toml` is the node's source when there is one, whatever
    // order the files came in.
    for p in projects.values_mut() {
        if p.declares() {
            p.manifest = Some(repo_path(
                &[p.dir.clone(), vec![PYPROJECT.to_owned()]].concat(),
            ));
        }
    }
    let systems: Vec<&Project> = projects.values().filter(|p| p.is_system()).collect();

    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, p) in systems.iter().enumerate() {
        if let Some(n) = p.name() {
            by_name.entry(normalise_name(&n)).or_default().push(i);
        }
    }
    let by_dir: BTreeMap<&[String], usize> = systems
        .iter()
        .enumerate()
        .map(|(i, p)| (p.dir.as_slice(), i))
        .collect();
    let unique = |n: &str| match by_name.get(n).map(Vec::as_slice) {
        Some([one]) => Some(*one),
        _ => None,
    };

    for p in &systems {
        let fallback = p.dir.last().cloned().unwrap_or_else(|| "(root)".to_owned());
        let source = p.manifest.clone().unwrap_or_else(RepoPath::root);
        let node = Node {
            id: node_id(&p.dir),
            kind: NodeKind::System,
            name: p.name().unwrap_or(fallback),
            source: source.clone(),
            parent: None,
            confidence: p.confidence(),
            visibility: None,
            code: None,
        };
        if let Err(e) = model.add_node(node) {
            diags.push(Diagnostic {
                path: source,
                message: e.to_string(),
            });
        }
    }

    let mut edges: BTreeMap<(usize, usize), Confidence> = BTreeMap::new();
    for (from, p) in systems.iter().enumerate() {
        let Some(t) = p.table() else { continue };
        let manifest = p.manifest.clone().unwrap_or_else(RepoPath::root);
        for dep in dependencies(t) {
            let target = match &dep.source {
                Some(Source::Path(rel)) => match normalise(&p.dir, rel) {
                    // A path to a directory that is no project: external.
                    Some(dir) => by_dir.get(dir.as_slice()).map(|&i| (i, Confidence::Exact)),
                    None => {
                        diags.push(Diagnostic {
                            path: manifest.clone(),
                            message: format!(
                                "`{}` has a path outside the repository: {rel}",
                                dep.name
                            ),
                        });
                        None
                    }
                },
                Some(Source::Workspace) => match unique(&dep.name) {
                    Some(i) => Some((i, Confidence::Exact)),
                    None => {
                        diags.push(Diagnostic {
                            path: manifest.clone(),
                            message: format!(
                                "`{}` is a workspace source but no single project is named so",
                                dep.name
                            ),
                        });
                        None
                    }
                },
                Some(Source::Elsewhere) => None,
                None => unique(&dep.name).map(|i| (i, Confidence::Inferred)),
            };
            if let Some((to, conf)) = target
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
                from: node_id(&systems[from].dir),
                to: node_id(&systems[to].dir),
                kind: EdgeKind::DependsOn,
            },
            confidence,
            evidence: BTreeSet::new(),
            dependency: BTreeSet::from([DepKind::Normal]),
        };
        if let Err(e) = model.add_edge(edge) {
            diags.push(Diagnostic {
                path: systems[from]
                    .manifest
                    .clone()
                    .unwrap_or_else(RepoPath::root),
                message: e.to_string(),
            });
        }
    }
    diags.sort();
    diags.dedup();
    diags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_normalise_as_pep_503() {
        assert_eq!(normalise_name("Foo.Bar__baz"), "foo-bar-baz");
        assert_eq!(normalise_name("a-b"), "a-b");
    }

    #[test]
    fn requirements_name_their_distribution() {
        assert_eq!(requirement_name("requests>=2"), Some("requests".into()));
        assert_eq!(
            requirement_name("My_Pkg[extra] ; python_version<'3.9'"),
            Some("my-pkg".into())
        );
        assert_eq!(requirement_name("pkg @ https://x/y.whl"), None);
        assert_eq!(requirement_name("pkg[e] @ file:///x"), None);
        assert_eq!(requirement_name(">=1"), None);
        assert_eq!(requirement_name("-"), None);
    }

    #[test]
    fn setup_cfg_names_are_plain_values() {
        assert_eq!(
            setup_cfg_name("[metadata]\nname = core\n"),
            Some("core".into())
        );
        assert_eq!(setup_cfg_name("[metadata]\nname = attr: pkg.NAME\n"), None);
        assert_eq!(setup_cfg_name("[options]\nname = x\n"), None);
    }
}
