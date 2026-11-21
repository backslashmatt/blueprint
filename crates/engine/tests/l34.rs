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
    (
        "web/src/view.tsx",
        "import { helper } from \"./util\";\nexport class View extends Base {\n  render() { return <div>{helper()}</div>; }\n}\nexport function mount() {}\n",
    ),
    ("web/src/util.js", "export function helper() {}\n"),
    ("web/assets/logo.svg", "<svg/>\n"),
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
    // Each TypeScript or JavaScript file of a package is a module; an
    // `index` file is its directory's.
    has("L2:npm:web#src");
    has("L2:npm:web#src/view");
    has("L3:npm:web#src/view/View");
    has("L4:npm:web#src/view/View/render");
    has("L4:npm:web#src/view/(functions)/mount");
    has("L4:npm:web#src/util/(functions)/helper");
    has("L3:npm:web#assets%2F/assets%2Flogo.svg");
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

#[test]
fn only_declared_crates_resolve_and_only_declared_dependencies_reach_l1() {
    let entries: &[(&str, &str)] = &[
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"b\", \"c\", \"t\"]\n",
        ),
        (
            "a/Cargo.toml",
            "[package]\nname = \"a\"\n[dependencies]\nbee = { package = \"b\", path = \"../b\" }\n[dev-dependencies]\nt = { path = \"../t\" }\n",
        ),
        (
            "a/src/lib.rs",
            "mod toml;\nmod inner;\nuse bee::Thing;\nuse c::Other;\nuse b::Thing as T2;\n#[cfg(test)]\nmod tests { use t::Fixture; }\n",
        ),
        ("a/src/toml.rs", "pub struct Value;\n"),
        ("a/src/inner.rs", "use toml::Value;\n"),
        (
            "b/Cargo.toml",
            "[package]\nname = \"b\"\n[lib]\nname = \"bee_lib\"\n",
        ),
        ("b/src/lib.rs", "pub struct Thing;\npub trait Show {}\n"),
        (
            "c/Cargo.toml",
            "[package]\nname = \"c\"\n[dependencies]\nb = { path = \"../b\" }\n",
        ),
        (
            "c/src/lib.rs",
            "pub struct Other;\nuse bee_lib::Show;\nimpl Show for Local {}\nstruct Local;\n",
        ),
        ("t/Cargo.toml", "[package]\nname = \"t\"\n"),
        ("t/src/lib.rs", "pub struct Fixture;\n"),
    ];
    let files = files(entries);
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let (model, _) = analyse_deep(&paths, &|p| files.get(p).cloned());
    let dep = EdgeKind::DependsOn;
    // A rename in the manifest is the name the code uses.
    assert_eq!(
        edge(&model, "L2:cargo:a#", "L2:cargo:b#", dep),
        Some(Confidence::Exact)
    );
    // `c` is a workspace member that `a` does not declare: no edge, and the
    // package name of a renamed dependency is not in scope either.
    assert!(
        model
            .edges()
            .all(|e| e.key.to.system_key().dir.to_string() != "c")
    );
    // A dev-dependency: the test module's edge exists, but `a` does not
    // depend on `t` as a system.
    assert_eq!(
        edge(&model, "L2:cargo:a#tests", "L2:cargo:t#", dep),
        Some(Confidence::Exact)
    );
    assert!(edge(&model, "L1:cargo:a#", "L1:cargo:t#", dep).is_none());
    // Under its `[lib] name`, `b` is found from `c`, and c's impl of b's
    // trait for its own type points c -> b, the dependency's direction.
    assert_eq!(
        edge(
            &model,
            "L3:cargo:c#Local",
            "L3:cargo:b#Show",
            EdgeKind::Implements
        ),
        Some(Confidence::Exact)
    );
    // A declared crate wins over a same-named root module: `toml` here is
    // not a declared crate, so the root module `toml` is used (2015 path).
    assert_eq!(
        edge(&model, "L2:cargo:a#inner", "L2:cargo:a#toml", dep),
        Some(Confidence::Exact)
    );
}

#[test]
fn an_impl_of_a_local_trait_for_a_foreign_type_gives_no_edge() {
    let entries: &[(&str, &str)] = &[
        ("Cargo.toml", "[workspace]\nmembers = [\"x\", \"y\"]\n"),
        ("x/Cargo.toml", "[package]\nname = \"x\"\n"),
        ("x/src/lib.rs", "pub struct Foreign;\n"),
        (
            "y/Cargo.toml",
            "[package]\nname = \"y\"\n[dependencies]\nx = { path = \"../x\" }\n",
        ),
        (
            "y/src/lib.rs",
            "use x::Foreign;\npub trait Mine {}\nimpl Mine for Foreign {}\n",
        ),
    ];
    let files = files(entries);
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let (model, _) = analyse_deep(&paths, &|p| files.get(p).cloned());
    assert!(model.edges().all(|e| e.key.kind != EdgeKind::Implements));
    assert!(edge(&model, "L1:cargo:x#", "L1:cargo:y#", EdgeKind::Implements).is_none());
}

#[test]
fn calls_resolve_by_path_and_roll_up() {
    let entries: &[(&str, &str)] = &[
        ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"lib\"]\n"),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\n[dependencies]\nlib = { path = \"../lib\" }\n",
        ),
        (
            "app/src/main.rs",
            "mod util;\nuse util::helper;\nuse lib::Graph;\nfn main() {\n    local();\n    helper();\n    util::other();\n    Graph::new();\n    lib::build();\n    main();\n    x.method();\n}\nfn local() {}\n",
        ),
        (
            "app/src/util.rs",
            "pub fn helper() {}\npub fn other() {}\npub struct Cfg;\nimpl Cfg {\n    pub fn load() -> Self { Self::parse() }\n    fn parse() -> Self { Cfg }\n}\n",
        ),
        ("lib/Cargo.toml", "[package]\nname = \"lib\"\n"),
        (
            "lib/src/lib.rs",
            "pub struct Graph;\nimpl Graph { pub fn new() -> Self { Graph } }\npub fn build() {}\n",
        ),
    ];
    let files = files(entries);
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let (model, _) = analyse_deep(&paths, &|p| files.get(p).cloned());
    let calls = |from: &str, to: &str| edge(&model, from, to, EdgeKind::Calls);
    let main = "L4:cargo:app#(functions)/main";
    let inf = Some(Confidence::Inferred);
    assert_eq!(calls(main, "L4:cargo:app#(functions)/local"), inf);
    assert_eq!(
        calls(main, "L4:cargo:app#util/(functions)/helper"),
        inf,
        "imported"
    );
    assert_eq!(
        calls(main, "L4:cargo:app#util/(functions)/other"),
        inf,
        "module path"
    );
    assert_eq!(calls(main, "L4:cargo:lib#Graph/new"), inf, "imported type");
    assert_eq!(
        calls(main, "L4:cargo:lib#(functions)/build"),
        inf,
        "crate path"
    );
    assert_eq!(
        calls("L4:cargo:app#util/Cfg/load", "L4:cargo:app#util/Cfg/parse"),
        inf,
        "Self::"
    );
    assert!(calls(main, main).is_none(), "recursion is not an edge");
    let from_main = model
        .edges()
        .filter(|e| String::from(e.key.from.clone()) == main && e.key.kind == EdgeKind::Calls)
        .count();
    assert_eq!(from_main, 5, "the method call is not resolved");
    // Rolled up to L3 and L2; calls never become system dependencies.
    assert_eq!(
        calls("L3:cargo:app#(functions)", "L3:cargo:app#util/(functions)"),
        inf
    );
    assert_eq!(calls("L2:cargo:app#", "L2:cargo:app#util"), inf);
    assert!(
        model
            .edges()
            .all(|e| !(e.key.kind == EdgeKind::Calls && e.key.from.level() == Level::System))
    );
}

#[test]
fn calls_through_globs_and_body_uses() {
    let entries: &[(&str, &str)] = &[
        ("Cargo.toml", "[package]\nname = \"one\"\n"),
        (
            "src/lib.rs",
            "mod m;\npub fn helper() {}\npub fn shadow() {\n    use crate::m::helper;\n    helper();\n}\nmod tests {\n    use super::*;\n    fn t() { helper(); }\n}\n",
        ),
        ("src/m.rs", "pub fn helper() {}\n"),
    ];
    let files = files(entries);
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let (model, _) = analyse_deep(&paths, &|p| files.get(p).cloned());
    let calls = |from: &str, to: &str| edge(&model, from, to, EdgeKind::Calls);
    assert!(
        calls(
            "L4:cargo:#(functions)/shadow",
            "L4:cargo:#m/(functions)/helper"
        )
        .is_some()
    );
    assert!(
        calls(
            "L4:cargo:#(functions)/shadow",
            "L4:cargo:#(functions)/helper"
        )
        .is_none()
    );
    assert!(
        calls(
            "L4:cargo:#tests/(functions)/t",
            "L4:cargo:#(functions)/helper"
        )
        .is_some()
    );
}
