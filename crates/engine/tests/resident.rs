//! The resident engine (DR 0008 decision 5): after any one-file change, its
//! model must equal a fresh build of the new tree, whichever path it took.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::analyse_deep;
use blueprint_engine::resident::{Resident, Update};
use blueprint_model::{EdgeKind, RepoPath};

use common::{Files, files};

const FIXTURE: &[(&str, &str)] = &[
    ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n"),
    ("core/Cargo.toml", "[package]\nname = \"core\"\n"),
    (
        "core/src/lib.rs",
        "pub mod graph;\nmod util;\n\npub fn top() {}\n\nfn helper() {}\n",
    ),
    (
        "core/src/graph.rs",
        "pub struct Graph;\n\nimpl Graph {\n    pub fn new() -> Self { Graph }\n}\n\nimpl Default for Graph {\n    fn default() -> Self { Self::new() }\n}\n",
    ),
    ("core/src/util.rs", "pub fn util() {}\n"),
    ("core/tests/it.rs", "#[test]\nfn it() {}\n"),
    (
        "app/Cargo.toml",
        "[package]\nname = \"app\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
    ),
    ("app/src/main.rs", "fn main() {}\n"),
];

/// Applies `text` to `path`, updates `r`, and checks the model against a
/// fresh build. Returns how the update went.
fn change(r: &mut Resident, tree: &mut Files, path: &str, text: &str) -> Update {
    let path = RepoPath::parse(path).unwrap();
    tree.insert(path.clone(), text.as_bytes().to_vec());
    let how = r.update(&path, &|p| tree.get(p).cloned());
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let (fresh, diags) = analyse_deep(&paths, &|p| tree.get(p).cloned());
    assert_eq!(*r.model(), fresh, "after changing {path} ({how:?})");
    assert_eq!(r.diagnostics(), diags.as_slice());
    how
}

fn calls(r: &Resident) -> Vec<String> {
    r.model()
        .edges()
        .filter(|e| e.key.kind == EdgeKind::Calls)
        .map(|e| format!("{} -> {}", e.key.from, e.key.to))
        .collect()
}

#[test]
fn edits_inside_items_are_applied_in_place_and_match_a_fresh_build() {
    let mut tree = files(FIXTURE);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    let before = calls(&r);

    // A new call, across modules: rolls up to L3 and L2.
    let lib = "pub mod graph;\nmod util;\n\npub fn top() { crate::graph::Graph::new(); util::util(); }\n\nfn helper() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::InPlace
    );
    let added = calls(&r);
    assert!(added.len() > before.len(), "{added:#?}");
    assert!(
        added
            .iter()
            .any(|c| c.starts_with("L2:") || c.starts_with("L3:")),
        "rolled up: {added:#?}"
    );

    // One call gone: its roll-up goes with it, the other's stays.
    let lib = "pub mod graph;\nmod util;\n\npub fn top() { util::util(); }\n\nfn helper() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::InPlace
    );

    // Lines move and a signature changes: still in place.
    let lib = "pub mod graph;\nmod util;\n\n\n\npub fn top(n: u32) { let _ = n; util::util(); }\n\nfn helper() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::InPlace
    );

    // Back to no calls: every roll-up is gone again.
    let lib = "pub mod graph;\nmod util;\n\npub fn top() {}\n\nfn helper() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::InPlace
    );
    assert_eq!(calls(&r), before);

    // Two callers share a roll-up; one stops calling, the roll-up stays.
    let lib = "pub mod graph;\nmod util;\n\npub fn top() { util::util(); }\n\nfn helper() { util::util(); }\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::InPlace
    );
    let lib = "pub mod graph;\nmod util;\n\npub fn top() {}\n\nfn helper() { util::util(); }\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::InPlace
    );
    assert!(
        calls(&r).iter().any(|c| c.starts_with("L2:")),
        "{:#?}",
        calls(&r)
    );

    // A body that calls a method through `Self`.
    let graph = "pub struct Graph;\n\nimpl Graph {\n    pub fn new() -> Self { Graph }\n}\n\nimpl Default for Graph {\n    fn default() -> Self { Graph }\n}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/graph.rs", graph),
        Update::InPlace
    );
}

#[test]
fn structural_edits_rebuild_and_match_a_fresh_build() {
    let mut tree = files(FIXTURE);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());

    // A new item.
    let util = "pub fn util() {}\npub fn more() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/util.rs", util),
        Update::Rebuilt
    );
    // A new reference.
    let util = "use crate::graph::Graph;\npub fn util() {}\npub fn more() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/util.rs", util),
        Update::Rebuilt
    );
    // A new module declaration (to a file that is not there).
    let lib = "pub mod graph;\nmod util;\nmod gone;\n\npub fn top() {}\n\nfn helper() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::Rebuilt
    );
    // Syntax errors change the file's parse state.
    let util = "use crate::graph::Graph;\npub fn util() { let x = ; }\npub fn more() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/util.rs", util),
        Update::Rebuilt
    );
    // A manifest, and a Rust file no module tree reaches.
    let manifest = "[package]\nname = \"app\"\n";
    assert_eq!(
        change(&mut r, &mut tree, "app/Cargo.toml", manifest),
        Update::Rebuilt
    );
    assert_eq!(
        change(
            &mut r,
            &mut tree,
            "core/tests/it.rs",
            "#[test]\nfn it2() {}\n"
        ),
        Update::Rebuilt
    );
}

/// This repository's own sources: shifting every line of a file is applied
/// in place and gives exactly a fresh build.
#[test]
fn this_repository_edits_match_a_fresh_build() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (dir, _) = blueprint_engine::source::Directory::open(&root).unwrap();
    let mut tree: Files = dir
        .paths()
        .iter()
        .filter(|p| {
            p.segments()
                .first()
                .is_some_and(|s| s == "crates" || s == "Cargo.toml")
        })
        .filter_map(|p| Some((p.clone(), dir.read(p)?)))
        .collect();
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    for file in [
        "crates/engine/src/l34.rs",
        "crates/model/src/graph.rs",
        "crates/cli/src/main.rs",
    ] {
        let path = RepoPath::parse(file).unwrap();
        let mut text = String::from_utf8(tree[&path].clone()).unwrap();
        text.insert_str(0, "\n\n");
        assert_eq!(
            change(&mut r, &mut tree, file, &text),
            Update::InPlace,
            "{file}"
        );
    }
}
