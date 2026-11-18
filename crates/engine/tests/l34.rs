//! L3–L4 analysis (DR 0008) over a fixture workspace, as a text snapshot in
//! `tests/snapshots/`. Set `UPDATE_SNAPSHOTS=1` to rewrite it.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::analyse_deep;
use blueprint_model::{Confidence, Level, Model, NodeKind, RepoPath};

use common::{files, render, snapshot};

fn deep(entries: &[(&str, &str)]) -> Model {
    let files = files(entries);
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let (model, diags) = analyse_deep(&paths, &|p| files.get(p).cloned());
    snapshot("l34_fixture", &render(&model, &diags));
    model
}

const FIXTURE: &[(&str, &str)] = &[
    ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"web\"]\n"),
    ("README.md", "# fixture\n"),
    ("core/Cargo.toml", "[package]\nname = \"core\"\n"),
    (
        "core/src/lib.rs",
        "pub mod graph;\nmod util;\n\n/// Root type.\npub struct Root;\n\npub fn top() {}\n\n#[cfg(unix)]\nfn twice() {}\n#[cfg(not(unix))]\nfn twice() {}\n",
    ),
    (
        "core/src/graph.rs",
        "pub struct Graph { nodes: Vec<u32> }\n\nimpl Graph {\n    pub fn new() -> Self { Graph { nodes: vec![] } }\n}\n\nimpl Default for Graph {\n    fn default() -> Self { Self::new() }\n}\n\npub trait Walk {\n    fn walk(&self);\n}\n\nimpl Walk for Graph {\n    fn walk(&self) {}\n}\n\nmod inner {\n    pub fn helper() {}\n}\n",
    ),
    (
        "core/src/util.rs",
        "use crate::graph::Graph;\n\nimpl Graph {\n    pub fn len(&self) -> usize { self.nodes.len() }\n}\n\nimpl Foreign {\n    fn odd(&self) {}\n}\n\npub struct Graph2;\nfn broken() { let x = ; }\n",
    ),
    ("core/tests/it.rs", "#[test]\nfn it() {}\n"),
    ("web/package.json", "{ \"name\": \"web\" }"),
    ("web/src/index.ts", "export const x = 1;\n"),
];

#[test]
fn types_functions_and_methods_sit_under_their_modules() {
    let model = deep(FIXTURE);
    let ids: Vec<String> = model
        .nodes()
        .filter(|n| n.id.level() >= Level::Component)
        .map(|n| String::from(n.id.clone()))
        .collect();
    let has = |id: &str| assert!(ids.iter().any(|i| i == id), "no {id} in {ids:#?}");
    // The crate root gets an L2 node of its own for its items.
    has("L3:cargo:core#Root");
    has("L4:cargo:core#(functions)/top");
    // Two cfg variants of one function: the second gets `~2`.
    has("L4:cargo:core#(functions)/twice");
    has("L4:cargo:core#(functions)/twice~2");
    // Methods sit under their type, even from another module's impl; a
    // trait impl's method is named for its trait.
    has("L4:cargo:core#graph/Graph/new");
    has("L4:cargo:core#graph/Graph/len");
    has("L4:cargo:core#graph/Graph/<Default>%3A%3Adefault");
    has("L4:cargo:core#graph/Graph/<Walk>%3A%3Awalk");
    has("L4:cargo:core#graph/Walk/walk");
    // A type not in the crate: an `impl` component in the impl's module.
    has("L3:cargo:core#util/impl Foreign");
    has("L4:cargo:core#graph/inner/(functions)/helper");
    // Files no analyser read: one `unknown` component each.
    has("L3:cargo:core#tests%2F/tests%2Fit.rs");
    has("L3:npm:web#src%2F/src%2Findex.ts");
    has("L3:unowned:#.%2F/README.md");
}

#[test]
fn confidence_follows_the_module_and_the_parse() {
    let model = deep(FIXTURE);
    let conf = |id: &str| {
        model
            .nodes()
            .find(|n| String::from(n.id.clone()) == id)
            .map(|n| n.confidence)
    };
    assert_eq!(conf("L3:cargo:core#graph/Graph"), Some(Confidence::Exact));
    assert_eq!(
        conf("L3:cargo:core#util/Graph2"),
        Some(Confidence::Inferred),
        "a file with syntax errors keeps its items, inferred"
    );
    assert_eq!(
        conf("L3:cargo:core#util/impl Foreign"),
        Some(Confidence::Inferred),
        "a method on a type outside the crate is placed by inference"
    );
    assert_eq!(
        conf("L4:cargo:core#graph/Graph/len"),
        Some(Confidence::Inferred)
    );
    let unknown = model
        .nodes()
        .filter(|n| {
            n.kind == NodeKind::Component
                && n.code.is_none()
                && !n.name.starts_with('(')
                && !n.name.starts_with("impl ")
        })
        .all(|n| n.confidence == Confidence::Unknown);
    assert!(unknown, "every file component is unknown");
}
