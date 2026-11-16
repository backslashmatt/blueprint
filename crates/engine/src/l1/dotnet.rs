//! L1 for .NET: one system per project file (`*.csproj`, `*.fsproj`,
//! `*.vbproj`).
//!
//! Project files are scanned for their `<Project>` root and
//! `<ProjectReference Include="..">` items; `dotnet` and MSBuild are never
//! run (§5.5). XML comments are skipped. Files under `bin` or `obj`
//! (build output) are ignored.
//!
//! - **Nodes:** every project file, named by its file stem. A file that is
//!   not UTF-8 or has no `<Project` element still becomes a node with
//!   `unknown` confidence. Two project files in one directory share an id,
//!   so the second is reported and skipped.
//! - **Edges:** each `ProjectReference` (`;`-separated lists allowed):
//!   - `exact`: a literal relative path to a project file in the repository;
//!   - `inferred`: a path built with MSBuild properties (`$(..)`) whose file
//!     name matches exactly one project, since the properties are not
//!     evaluated.

use std::collections::{BTreeMap, BTreeSet};

use blueprint_model::{
    Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Model, Node, NodeId, NodeKind,
    RepoPath, SystemKey,
};

use super::path::{Dir, normalise, repo_path};
use super::{Diagnostic, Read};

const EXTENSIONS: [&str; 3] = [".csproj", ".fsproj", ".vbproj"];

struct Project {
    dir: Dir,
    file: RepoPath,
    name: String,
    ok: bool,
    references: Vec<String>,
}

fn node_id(dir: &[String]) -> NodeId {
    NodeId::system(SystemKey {
        ecosystem: Ecosystem::Dotnet,
        dir: repo_path(dir),
    })
}

fn is_project(p: &RepoPath) -> bool {
    let segs = p.segments();
    let Some(file) = segs.last() else {
        return false;
    };
    let lower = file.to_ascii_lowercase();
    EXTENSIONS
        .iter()
        .any(|e| lower.ends_with(e) && lower.len() > e.len())
        && !segs[..segs.len() - 1]
            .iter()
            .any(|s| s.eq_ignore_ascii_case("bin") || s.eq_ignore_ascii_case("obj"))
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i + 4..].find("-->") {
            Some(j) => rest = &rest[i + 4 + j + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` in a tag's attribute text, either quote.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(&name.to_ascii_lowercase()) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let after = tag[from..].trim_start();
        if !before_ok || !after.starts_with('=') {
            continue;
        }
        let value = after[1..].trim_start();
        let quote = value.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let end = value[1..].find(quote)?;
        return Some(value[1..1 + end].to_owned());
    }
    None
}

/// The `Include` values of every `<ProjectReference ..>` element, and
/// whether a `<Project` root element is present.
fn scan(text: &str) -> (bool, Vec<String>) {
    let text = strip_comments(text);
    let lower = text.to_ascii_lowercase();
    // `<Project` followed by whitespace, `>` or `/`, not `<ProjectReference`.
    let has_project = lower.match_indices("<project").any(|(i, m)| {
        lower[i + m.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '>' || c == '/')
    });
    let mut refs = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<projectreference") {
        let start = from + i + "<projectreference".len();
        let end = lower[start..].find('>').map_or(lower.len(), |j| start + j);
        if let Some(include) = attribute(&text[start..end], "Include") {
            refs.extend(
                include
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        }
        from = end;
    }
    (has_project, refs)
}

/// Adds the .NET systems found among `paths` to `model`, and returns what
/// could not be fully understood. Never fails: problems degrade nodes.
pub fn analyse(paths: &[RepoPath], read: Read<'_>, model: &mut Model) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut projects: Vec<Project> = Vec::new();
    for path in paths.iter().filter(|p| is_project(p)) {
        let segs = path.segments();
        let dir: Dir = segs[..segs.len() - 1].to_vec();
        let file = &segs[segs.len() - 1];
        let name = file
            .rsplit_once('.')
            .map_or(file.as_str(), |(stem, _)| stem)
            .to_owned();
        let text = read(path).and_then(|b| String::from_utf8(b).ok());
        let text = text.map(|t| t.strip_prefix('\u{feff}').map(str::to_owned).unwrap_or(t));
        let (ok, references) = match &text {
            Some(t) => scan(t),
            None => (false, Vec::new()),
        };
        if !ok {
            let why = if text.is_none() {
                "cannot be read as UTF-8"
            } else {
                "no <Project> element"
            };
            diags.push(Diagnostic {
                path: path.clone(),
                message: why.into(),
            });
        }
        projects.push(Project {
            dir,
            file: path.clone(),
            name,
            ok,
            references,
        });
    }

    let mut added: Vec<usize> = Vec::new();
    let mut seen: BTreeSet<&[String]> = BTreeSet::new();
    for (i, p) in projects.iter().enumerate() {
        if !seen.insert(p.dir.as_slice()) {
            diags.push(Diagnostic {
                path: p.file.clone(),
                message: "another project file in this directory comes first".into(),
            });
            continue;
        }
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
        match model.add_node(node) {
            Ok(()) => added.push(i),
            Err(e) => diags.push(Diagnostic {
                path: p.file.clone(),
                message: e.to_string(),
            }),
        }
    }
    let by_file: BTreeMap<&RepoPath, usize> =
        added.iter().map(|&i| (&projects[i].file, i)).collect();
    let mut by_file_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for &i in &added {
        if let Some(f) = projects[i].file.segments().last() {
            by_file_name
                .entry(f.to_ascii_lowercase())
                .or_default()
                .push(i);
        }
    }

    let mut edges: BTreeMap<(usize, usize), Confidence> = BTreeMap::new();
    for &from in &added {
        let p = &projects[from];
        for r in &p.references {
            let found = if r.contains("$(") {
                let file = r
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match by_file_name.get(&file).map(Vec::as_slice) {
                    Some([one]) => Some((*one, Confidence::Inferred)),
                    _ => None,
                }
            } else {
                match normalise(&p.dir, r) {
                    None => {
                        diags.push(Diagnostic {
                            path: p.file.clone(),
                            message: format!("reference {r} is outside the repository"),
                        });
                        None
                    }
                    Some(target) => match by_file.get(&repo_path(&target)) {
                        Some(&i) => Some((i, Confidence::Exact)),
                        None => {
                            diags.push(Diagnostic {
                                path: p.file.clone(),
                                message: format!("reference {r} names no project file"),
                            });
                            None
                        }
                    },
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
