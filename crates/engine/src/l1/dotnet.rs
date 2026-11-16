//! L1 for .NET: one system per project file (`*.csproj`, `*.fsproj`,
//! `*.vbproj`).
//!
//! Project files are read with a small XML tag reader (quoted attribute
//! values, comments, CDATA and entities handled) for their `<Project>` root
//! and `<ProjectReference Include=".." Remove="..">` items; `dotnet` and
//! MSBuild are never run (§5.5), so imports such as `Directory.Build.props`
//! and solution files are not read. Files under `bin` or `obj` (build
//! output) are ignored.
//!
//! - **Nodes:** every project file, named by its file stem. A file that is
//!   not UTF-8 or has no `<Project>` element still becomes a node with
//!   `unknown` confidence. Two project files in one directory share an id:
//!   the first by path becomes the node, and references to either resolve
//!   to it.
//! - **Edges:** each `ProjectReference` (`;`-separated lists allowed):
//!   - `exact`: a literal relative path to a project file in the repository
//!     (matched case-insensitively if the exact case is not found, as on
//!     Windows), unless a later `Remove` drops it;
//!   - `inferred`: a path built with MSBuild properties (`$(..)`) whose file
//!     name matches exactly one project, since the properties are not
//!     evaluated;
//!   - wildcard `Include`s are not expanded and are reported.

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

/// Decodes the five XML entities and numeric character references.
fn decode(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|&e| e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One start tag: its element name and attributes (values decoded).
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
}

/// Every start tag in `text`, skipping comments, CDATA, processing
/// instructions and declarations. Attribute values may contain `>`.
fn tags(text: &str) -> Vec<Tag> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find('<') {
        i += off;
        let rest = &text[i..];
        let skip_to = |end: &str| rest.find(end).map_or(text.len(), |e| i + e + end.len());
        if rest.starts_with("<!--") {
            i = skip_to("-->");
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = skip_to("]]>");
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") || rest.starts_with("</") {
            i = skip_to(">");
            continue;
        }
        // Element name.
        let mut j = i + 1;
        while j < b.len() && !(b[j].is_ascii_whitespace() || b[j] == b'>' || b[j] == b'/') {
            j += 1;
        }
        let name = text[i + 1..j].to_owned();
        // Attributes until `>` outside quotes.
        let mut attrs = Vec::new();
        loop {
            while j < b.len() && (b[j].is_ascii_whitespace() || b[j] == b'/') {
                j += 1;
            }
            if j >= b.len() || b[j] == b'>' {
                break;
            }
            let start = j;
            while j < b.len()
                && !(b[j].is_ascii_whitespace() || b[j] == b'=' || b[j] == b'>' || b[j] == b'/')
            {
                j += 1;
            }
            let attr = text[start..j].to_owned();
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if j >= b.len() || b[j] != b'=' {
                if j == start {
                    j += 1;
                }
                continue;
            }
            j += 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let Some(&q) = b.get(j).filter(|&&q| q == b'"' || q == b'\'') else {
                continue;
            };
            let vstart = j + 1;
            let vend = text[vstart..]
                .find(q as char)
                .map_or(text.len(), |e| vstart + e);
            attrs.push((attr, decode(&text[vstart..vend])));
            j = (vend + 1).min(b.len());
        }
        out.push(Tag { name, attrs });
        i = (j + 1).min(text.len());
        if j >= b.len() {
            break;
        }
    }
    out
}

/// Whether there is a `<Project>` element, and the project references that
/// remain after `Remove`s, plus any wildcard `Include`s (not expanded).
fn scan(text: &str) -> (bool, Vec<String>, Vec<String>) {
    let mut has_project = false;
    let (mut refs, mut wild): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut removed: Vec<String> = Vec::new();
    let split = |v: &str| -> Vec<String> {
        v.split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    for tag in tags(text) {
        if tag.name.eq_ignore_ascii_case("Project") {
            has_project = true;
        } else if tag.name.eq_ignore_ascii_case("ProjectReference") {
            for (attr, value) in &tag.attrs {
                if attr.eq_ignore_ascii_case("Include") {
                    for r in split(value) {
                        if r.contains(['*', '?']) {
                            wild.push(r)
                        } else {
                            refs.push(r)
                        }
                    }
                } else if attr.eq_ignore_ascii_case("Remove") {
                    removed.extend(split(value));
                }
            }
        }
    }
    let key = |r: &str| r.replace('\\', "/").to_ascii_lowercase();
    let removed: BTreeSet<String> = removed.iter().map(|r| key(r)).collect();
    refs.retain(|r| !removed.contains(&key(r)));
    (has_project, refs, wild)
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
        let (ok, references, wild) = match &text {
            Some(t) => scan(t),
            None => (false, Vec::new(), Vec::new()),
        };
        for w in wild {
            diags.push(Diagnostic {
                path: path.clone(),
                message: format!("wildcard reference {w} is not expanded"),
            });
        }
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

    // Input order must not decide which file of a shared directory wins.
    projects.sort_by(|a, b| a.file.cmp(&b.file));
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
    // Every project file, including a second one in a shared directory,
    // resolves to the node of its directory.
    let winner: BTreeMap<&[String], usize> = added
        .iter()
        .map(|&i| (projects[i].dir.as_slice(), i))
        .collect();
    let mut by_file: BTreeMap<&RepoPath, usize> = BTreeMap::new();
    let mut by_lower: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    let mut by_file_name: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for p in &projects {
        let Some(&w) = winner.get(p.dir.as_slice()) else {
            continue;
        };
        by_file.insert(&p.file, w);
        by_lower
            .entry(p.file.to_string().to_ascii_lowercase())
            .or_default()
            .insert(w);
        if let Some(f) = p.file.segments().last() {
            by_file_name
                .entry(f.to_ascii_lowercase())
                .or_default()
                .insert(w);
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
                match by_file_name.get(&file) {
                    Some(set) if set.len() == 1 => set.first().map(|&i| (i, Confidence::Inferred)),
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
                    Some(target) => match by_file.get(&repo_path(&target)).copied().or_else(|| {
                        // Windows and macOS resolve references case-insensitively.
                        let set =
                            by_lower.get(&repo_path(&target).to_string().to_ascii_lowercase())?;
                        (set.len() == 1).then(|| set.first().copied()).flatten()
                    }) {
                        Some(i) => Some((i, Confidence::Exact)),
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
