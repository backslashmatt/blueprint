//! Helpers shared by the analysis tests: fixtures, rendering and snapshots.

#![allow(dead_code, clippy::unwrap_used, clippy::panic)] // test code

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use blueprint_engine::l1::Diagnostic;
use blueprint_model::{Model, RepoPath};

pub type Files = BTreeMap<RepoPath, Vec<u8>>;

pub fn files(entries: &[(&str, &str)]) -> Files {
    entries
        .iter()
        .map(|(p, c)| (RepoPath::parse(p).unwrap(), c.as_bytes().to_vec()))
        .collect()
}

pub fn render(model: &Model, diags: &[Diagnostic]) -> String {
    let mut out = String::from("nodes:\n");
    for n in model.nodes() {
        out += &format!(
            "  {}  {:?}  {:?}  {:?}  {}\n",
            String::from(n.id.clone()),
            n.kind,
            n.name,
            n.confidence,
            n.source
        );
    }
    out += "edges:\n";
    for e in model.edges() {
        out += &format!(
            "  {} -> {}  {:?}  {:?}\n",
            String::from(e.key.from.clone()),
            String::from(e.key.to.clone()),
            e.key.kind,
            e.confidence
        );
    }
    out += "diagnostics:\n";
    for d in diags {
        out += &format!("  {}: {}\n", d.path, d.message.lines().next().unwrap_or(""));
    }
    out
}

pub fn snapshot(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/snapshots/{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "no snapshot {}; run with UPDATE_SNAPSHOTS=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "snapshot {name} differs; run with UPDATE_SNAPSHOTS=1 if intended"
    );
}

/// Every file in this checkout that `keep` accepts by name, skipping build output, hidden
/// directories and the fetched reference repositories.
pub fn this_repository(keep: impl Fn(&str) -> bool) -> Files {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = Files::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        for entry in std::fs::read_dir(root.join(&rel)).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = rel.join(&name);
            if entry.file_type().unwrap().is_dir() {
                if !name.starts_with('.') && name != "target" && child != Path::new("bench/repos") {
                    stack.push(child);
                }
            } else if keep(&name) {
                let p: Vec<String> = child
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.insert(
                    RepoPath::parse(&p.join("/")).unwrap(),
                    std::fs::read(root.join(&child)).unwrap(),
                );
            }
        }
    }
    out
}
