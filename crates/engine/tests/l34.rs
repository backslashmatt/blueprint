//! L3–L4 analysis (DR 0008) over a fixture workspace, as a text snapshot in
//! `tests/snapshots/`. Set `UPDATE_SNAPSHOTS=1` to rewrite it.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::analyse_deep;
use blueprint_model::{Confidence, EdgeKind, Level, Model, NodeKind, RepoPath};

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
    (
        "core/src/main.rs",
        "struct Args;\nimpl Args { fn parse() {} }\nfn main() {}\n",
    ),
    (
        "core/src/bin/tool.rs",
        "struct Args;\nimpl Args { fn parse() {} }\nfn main() {}\n",
    ),
    ("web/package.json", "{ \"name\": \"web\" }"),
    ("web/src/index.ts", "export const x = 1;\n"),
];

#[test]
fn types_functions_and_methods_sit_under_their_modules() {
    let model = deep(FIXTURE);
    let ids: Vec<String> = model
        .nodes()
        .filter(|n| n.id.level() >= Level::Module)
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
    // An impl in another module does not reach into the type's module.
    has("L4:cargo:core#util/impl Graph/len");
    // Each binary besides the primary crate has its own namespace.
    has("L2:cargo:core#");
    has("L2:cargo:core#(bin main)");
    has("L3:cargo:core#(bin main)/Args");
    has("L4:cargo:core#(bin main)/Args/parse");
    has("L4:cargo:core#(bin tool)/Args/parse");
    has("L4:cargo:core#(bin tool)/(functions)/main");
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
        conf("L4:cargo:core#util/impl Graph/len"),
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

/// This repository at L3–L4: every Rust item is placed, with no node
/// rejected by the model. Not snapshotted: it changes with every function.
#[test]
fn this_repository_l3_l4_model() {
    let repo = common::this_repository(|_| true);
    let paths: Vec<RepoPath> = repo.keys().cloned().collect();
    let (model, diags) = analyse_deep(&paths, &|p| repo.get(p).cloned());
    let rejected: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("node") || d.message.contains("parent"))
        .collect();
    assert!(rejected.is_empty(), "{rejected:?}");
    assert!(model.level(Level::Component).count() > 100);
    assert!(model.level(Level::Code).count() > 300);
    let analyse = model
        .nodes()
        .find(|n| String::from(n.id.clone()) == "L4:cargo:crates/engine#(functions)/analyse_deep");
    assert!(analyse.is_some(), "engine::analyse_deep is an L4 node");
    // Every crate-to-crate `use` rolls up onto a dependency Cargo declared:
    // an L1 depends-on edge with no manifest kind would be an undeclared
    // dependency, or a resolution bug.
    let undeclared: Vec<String> = model
        .edges()
        .filter(|e| e.key.from.level() == Level::System && e.key.kind == EdgeKind::DependsOn)
        .filter(|e| e.dependency.is_empty())
        .map(|e| {
            format!(
                "{} -> {}",
                String::from(e.key.from.clone()),
                String::from(e.key.to.clone())
            )
        })
        .collect();
    assert!(undeclared.is_empty(), "{undeclared:?}");
    let rolled = model
        .edges()
        .filter(|e| e.key.from.level() == Level::System && !e.evidence.is_empty())
        .count();
    assert!(rolled > 5, "crate-to-crate uses roll up onto the L1 edges");
}

fn edge(model: &Model, from: &str, to: &str, kind: EdgeKind) -> Option<Confidence> {
    model
        .edges()
        .find(|e| {
            String::from(e.key.from.clone()) == from
                && String::from(e.key.to.clone()) == to
                && e.key.kind == kind
        })
        .map(|e| e.confidence)
}

#[test]
fn uses_and_impls_become_edges_and_roll_up() {
    let entries: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"app\", \"core-lib\"]\n",
        ),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\n[dependencies]\ncore-lib = { path = \"../core-lib\" }\n",
        ),
        (
            "app/src/main.rs",
            "mod cli;\nuse core_lib::graph::Graph;\nuse std::fmt;\nfn main() {}\n",
        ),
        (
            "app/src/cli.rs",
            "use super::*;\nuse crate::cli as me;\nuse core_lib::graph::Missing;\npub struct Args;\n",
        ),
        (
            "app/src/bin/tool/main.rs",
            "mod opts;\nuse crate::opts::Opts;\nfn main() {}\n",
        ),
        ("app/src/bin/tool/opts.rs", "pub struct Opts;\n"),
        ("core-lib/Cargo.toml", "[package]\nname = \"core-lib\"\n"),
        (
            "core-lib/src/lib.rs",
            "pub mod graph;\npub mod walk;\nmod util;\n",
        ),
        ("core-lib/src/graph.rs", "pub struct Graph;\n"),
        ("core-lib/src/walk.rs", "pub trait Walk {}\n"),
        (
            "core-lib/src/util.rs",
            "use crate::walk::Walk;\nuse crate::graph;\nimpl Walk for graph::Graph {}\nimpl std::fmt::Debug for Local {}\nstruct Local;\n",
        ),
    ];
    let files = files(entries);
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let (model, _) = analyse_deep(&paths, &|p| files.get(p).cloned());
    let dep = EdgeKind::DependsOn;
    // Across crates, exact when the item exists, inferred when it does not.
    assert_eq!(
        edge(&model, "L2:cargo:app#", "L2:cargo:core-lib#graph", dep),
        Some(Confidence::Exact)
    );
    assert_eq!(
        edge(&model, "L2:cargo:app#cli", "L2:cargo:core-lib#graph", dep),
        Some(Confidence::Inferred)
    );
    // `super::*` reaches the parent module; `crate::cli as me` is the
    // module itself, so no edge.
    assert_eq!(
        edge(&model, "L2:cargo:app#cli", "L2:cargo:app#", dep),
        Some(Confidence::Exact)
    );
    assert!(edge(&model, "L2:cargo:app#cli", "L2:cargo:app#cli", dep).is_none());
    // `crate` inside a binary is that binary's root.
    assert_eq!(
        edge(
            &model,
            "L2:cargo:app#(bin tool)",
            "L2:cargo:app#(bin tool)/opts",
            dep
        ),
        Some(Confidence::Exact)
    );
    // std is outside the model.
    assert!(
        model
            .edges()
            .all(|e| !String::from(e.key.to.clone()).contains("std"))
    );
    // implements through imports, rolled up to L2.
    assert_eq!(
        edge(
            &model,
            "L3:cargo:core-lib#graph/Graph",
            "L3:cargo:core-lib#walk/Walk",
            EdgeKind::Implements
        ),
        Some(Confidence::Exact)
    );
    let rolled = model
        .edges()
        .find(|e| {
            String::from(e.key.from.clone()) == "L2:cargo:core-lib#graph"
                && String::from(e.key.to.clone()) == "L2:cargo:core-lib#walk"
        })
        .map(|e| e.evidence.len());
    assert_eq!(rolled, Some(1), "the L2 roll-up lists its L3 evidence");
    // The cross-crate L2 edges roll up onto the Cargo L1 edge.
    let l1 = model
        .edges()
        .find(|e| {
            String::from(e.key.from.clone()) == "L1:cargo:app#"
                && String::from(e.key.to.clone()) == "L1:cargo:core-lib#"
                && e.key.kind == dep
        })
        .map(|e| (e.confidence, e.evidence.len(), e.dependency.len()));
    assert_eq!(l1, Some((Confidence::Exact, 2, 1)));
}
