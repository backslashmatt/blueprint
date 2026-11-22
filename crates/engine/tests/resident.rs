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
    check(r, tree, &path)
}

/// Deletes `path`, updates `r`, and checks it as [`change`] does.
fn delete(r: &mut Resident, tree: &mut Files, path: &str) -> Update {
    let path = RepoPath::parse(path).unwrap();
    tree.remove(&path);
    check(r, tree, &path)
}

fn check(r: &mut Resident, tree: &Files, path: &RepoPath) -> Update {
    let how = r.update(path, &|p| tree.get(p).cloned());
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
fn structural_edits_match_a_fresh_build() {
    let mut tree = files(FIXTURE);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());

    // A new item, then a new reference: in place (#81).
    let util = "pub fn util() {}\npub fn more() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/util.rs", util),
        Update::Items
    );
    let util = "use crate::graph::Graph;\npub fn util() {}\npub fn more() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/util.rs", util),
        Update::Items
    );
    // A new module declaration (to a file that is not there).
    let lib = "pub mod graph;\nmod util;\nmod gone;\n\npub fn top() {}\n\nfn helper() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::Rebuilt
    );
    // Syntax errors change the file's parse state, and its items'
    // confidence with it.
    let util = "use crate::graph::Graph;\npub fn util() { let x = ; }\npub fn more() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/util.rs", util),
        Update::Items
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

#[test]
fn created_and_deleted_files_rebuild_and_match_a_fresh_build() {
    let mut tree = files(FIXTURE);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    let lib = "pub mod graph;\nmod util;\nmod extra;\n\npub fn top() {}\n\nfn helper() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/lib.rs", lib),
        Update::Rebuilt
    );
    let extra = "pub fn e() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "core/src/extra.rs", extra),
        Update::Rebuilt
    );
    assert_eq!(
        delete(&mut r, &mut tree, "core/src/util.rs"),
        Update::Rebuilt
    );
    assert_eq!(
        delete(&mut r, &mut tree, "core/src/extra.rs"),
        Update::Rebuilt
    );
    // Deleting what is already gone changes nothing.
    assert_eq!(
        delete(&mut r, &mut tree, "core/src/extra.rs"),
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
        "crates/engine/src/diff.rs",
    ] {
        let path = RepoPath::parse(file).unwrap();
        let original = String::from_utf8(tree[&path].clone()).unwrap();
        let mut text = original.clone();
        text.insert_str(0, "\n\n");
        assert_eq!(
            change(&mut r, &mut tree, file, &text),
            Update::InPlace,
            "{file}"
        );
        // A new function, then the file as it was: both in place (#81).
        let added = format!("{original}\npub fn added_by_the_test() {{ main(); }}\n");
        assert_eq!(
            change(&mut r, &mut tree, file, &added),
            Update::Items,
            "{file}"
        );
        assert_eq!(
            change(&mut r, &mut tree, file, &original),
            Update::Items,
            "{file}"
        );
    }
    // A crate's root file too (#141).
    let main = "crates/cli/src/main.rs";
    let original = String::from_utf8(tree[&RepoPath::parse(main).unwrap()].clone()).unwrap();
    let added = format!("{original}\npub fn added_by_the_test() {{}}\n");
    assert_eq!(change(&mut r, &mut tree, main, &added), Update::Items);
    assert_eq!(change(&mut r, &mut tree, main, &original), Update::Items);
}

#[test]
fn item_edits_to_a_crate_root_are_applied_by_item() {
    let mut tree = files(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n"),
        ("core/Cargo.toml", "[package]\nname = \"core\"\n"),
        // Only modules at first: the root's L2 node exists only once
        // something needs it.
        ("core/src/lib.rs", "pub mod graph;\n"),
        (
            "core/src/graph.rs",
            "pub struct Graph;\nimpl crate::Root { pub fn from_graph() {} }\n",
        ),
        ("core/src/main.rs", "fn main() {}\n"),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
        ),
        ("app/src/main.rs", "fn main() { core::top(); }\n"),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    let lib = "core/src/lib.rs";
    // The root's first items: a type another file has an impl for, and the
    // function another crate calls.
    let with_items = "pub mod graph;\npub struct Root;\npub fn top() {}\n";
    assert_eq!(change(&mut r, &mut tree, lib, with_items), Update::Items);
    assert!(
        calls(&r).iter().any(|c| c.contains("core#(functions)/top")),
        "{:?}",
        calls(&r)
    );
    // A renamed function, and then none again: the root's L2 node goes when
    // nothing needs it.
    let renamed = "pub mod graph;\npub struct Root;\npub fn other() {}\n";
    assert_eq!(change(&mut r, &mut tree, lib, renamed), Update::Items);
    assert_eq!(
        change(&mut r, &mut tree, lib, "pub mod graph;\n"),
        Update::Items
    );
    // The binary's root, beside the library.
    let main = "core/src/main.rs";
    assert_eq!(
        change(&mut r, &mut tree, main, "fn main() {}\nfn helper() {}\n"),
        Update::Items
    );
    assert_eq!(
        change(&mut r, &mut tree, main, "fn main() {}\n"),
        Update::Items
    );
    // Another crate reaching the root's items through an alias of the
    // crate, and through a glob: each must follow a rename.
    let app = "app/src/main.rs";
    let alias = "use core as c;\nfn main() { c::top(); }\n";
    change(&mut r, &mut tree, app, alias);
    change(&mut r, &mut tree, lib, with_items);
    change(&mut r, &mut tree, lib, renamed);
    let glob = "use core::*;\nfn main() { top(); other(); }\n";
    change(&mut r, &mut tree, app, glob);
    change(&mut r, &mut tree, lib, with_items);
    change(&mut r, &mut tree, lib, renamed);
    // Methods of a root type and items of a root's inline module, reached
    // by path and through a `use` (#143 review).
    let methods = "pub mod graph;\npub struct Root;\npub struct S;\nimpl S { pub fn new() {} }\npub mod m { pub fn f() {} }\n";
    change(&mut r, &mut tree, lib, methods);
    let paths_app = "use core::S;\nuse core::m;\nfn main() { core::S::new(); S::new(); core::m::g(); m::g(); }\n";
    change(&mut r, &mut tree, app, paths_app);
    let renamed_method = "pub mod graph;\npub struct Root;\npub struct S;\nimpl S { pub fn old() {} }\npub mod m { pub fn f() {} pub fn g() {} }\n";
    assert_eq!(
        change(&mut r, &mut tree, lib, renamed_method),
        Update::Items
    );
    assert_eq!(change(&mut r, &mut tree, lib, methods), Update::Items);
    let used = "use core::m::g;\nfn main() { g(); }\n";
    change(&mut r, &mut tree, app, used);
    change(&mut r, &mut tree, lib, renamed_method);
    change(&mut r, &mut tree, lib, methods);
    // A `mod` added to the root still rebuilds: the module tree changes.
    let more = "pub mod graph;\nmod extra;\n";
    assert_eq!(change(&mut r, &mut tree, lib, more), Update::Rebuilt);
}

/// Structural edits whose effects reach other files: calls into a removed
/// function, an impl of a renamed type, and a `use` whose confidence
/// depends on whether its item exists.
#[test]
fn structural_edits_reach_the_files_that_mention_them() {
    const TREE: &[(&str, &str)] = &[
        ("Cargo.toml", "[package]\nname = \"app\"\n"),
        ("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
        (
            "src/a.rs",
            "pub fn f() {}\npub fn g() {}\npub struct S;\npub trait T {}\n",
        ),
        (
            "src/b.rs",
            "use crate::a::f;\npub fn h() { crate::a::f(); crate::a::g(); }\n",
        ),
        (
            "src/c.rs",
            "use crate::a::{S, T};\nimpl T for S {}\npub fn k() { crate::a::S::new(); }\n",
        ),
    ];
    let mut tree = files(TREE);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    let edits = [
        // `f` goes: b's call and its exact `use` change.
        ("src/a.rs", "pub fn g() {}\npub struct S;\npub trait T {}\n"),
        // It comes back, with `S::new` that c calls.
        (
            "src/a.rs",
            "pub fn f() {}\npub fn g() {}\npub struct S;\nimpl S { pub fn new() -> Self { S } }\npub trait T {}\n",
        ),
        // `S` is renamed: c's impl and call no longer resolve.
        (
            "src/a.rs",
            "pub fn f() {}\npub fn g() {}\npub struct R;\nimpl R { pub fn new() -> Self { R } }\npub trait T {}\n",
        ),
        // b stops calling g and starts calling f twice.
        (
            "src/b.rs",
            "use crate::a::f;\npub fn h() { f(); crate::a::f(); }\npub fn i() {}\n",
        ),
        // c's own use changes.
        (
            "src/c.rs",
            "use crate::a::{R, T};\nimpl T for R {}\npub fn k() { crate::a::R::new(); }\n",
        ),
        // Everything goes from a.
        ("src/a.rs", ""),
    ];
    for (path, text) in edits {
        let how = change(&mut r, &mut tree, path, text);
        assert!(
            matches!(how, Update::Items | Update::InPlace),
            "{path}: {how:?}"
        );
    }
}

/// The paths that reach a file without naming every module from the crate
/// root: `super::`, a child path from an ancestor, a `use … as` alias, a
/// glob, and another crate.
#[test]
fn structural_edits_reach_relative_aliased_and_glob_paths() {
    const TREE: &[(&str, &str)] = &[
        ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n"),
        ("core/Cargo.toml", "[package]\nname = \"core\"\n"),
        ("core/src/lib.rs", "pub mod p;\n"),
        (
            "core/src/p.rs",
            "pub mod q;\npub mod sib;\npub fn up() { q::target(); }\n",
        ),
        ("core/src/p/q.rs", "pub mod child;\npub fn other() {}\n"),
        ("core/src/p/q/child.rs", "pub fn c() { super::target(); }\n"),
        (
            "core/src/p/sib.rs",
            "use super::q as alias;\nuse super::q::*;\npub fn s() { alias::target(); target(); super::q::target(); }\n",
        ),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
        ),
        ("app/src/main.rs", "mod m;\nfn main() {}\n"),
        ("app/src/m.rs", "pub fn x() { core::p::q::target(); }\n"),
    ];
    let mut tree = files(TREE);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    let target = "core/src/p/q.rs";
    // `target` appears, every caller's edge with it; then it goes again.
    for text in [
        "pub mod child;\npub fn other() {}\npub fn target() {}\n",
        "pub mod child;\npub fn other() {}\n",
        "pub mod child;\npub fn target() {}\npub struct target;\n",
    ] {
        let how = change(&mut r, &mut tree, target, text);
        assert_eq!(how, Update::Items, "{text}");
    }
    let calls: Vec<String> = r
        .model()
        .edges()
        .filter(|e| {
            e.key.kind == blueprint_model::EdgeKind::Calls
                && e.key.from.level() == blueprint_model::Level::Code
        })
        .map(|e| format!("{} -> {}", e.key.from, e.key.to))
        .collect();
    assert!(calls.len() >= 4, "every path reached `target`: {calls:#?}");
}

/// Edits that keep every name but change what it is (from review of #87).
#[test]
fn a_name_that_changes_kind_or_owner_reaches_its_callers() {
    let cases: &[(&str, &str, &str)] = &[
        // A function becomes a type.
        (
            "pub fn S() {}\n",
            "pub fn h() { crate::a::S(); }\n",
            "pub struct S {}\n",
        ),
        // A free function becomes a method.
        (
            "pub struct S;\npub fn new() {}\n",
            "pub fn h() { crate::a::new(); crate::a::S::new(); }\n",
            "pub struct S;\nimpl S { pub fn new() {} }\n",
        ),
        // An inherent method moves into a trait impl.
        (
            "pub struct S;\npub trait T {}\nimpl S { pub fn m() {} }\n",
            "pub fn h() { crate::a::S::m(); }\n",
            "pub struct S;\npub trait T {}\nimpl T for S { fn m() {} }\n",
        ),
        // A method moves to another type.
        (
            "pub struct S;\npub struct R;\nimpl S { pub fn m() {} }\n",
            "pub fn h() { crate::a::S::m(); crate::a::R::m(); }\n",
            "pub struct S;\npub struct R;\nimpl R { pub fn m() {} }\n",
        ),
    ];
    for (a, b, edit) in cases {
        let entries = [
            ("Cargo.toml", "[package]\nname = \"app\"\n"),
            ("src/lib.rs", "mod a;\nmod b;\n"),
            ("src/a.rs", *a),
            ("src/b.rs", *b),
        ];
        let mut tree = files(&entries);
        let paths: Vec<RepoPath> = tree.keys().cloned().collect();
        let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
        assert_eq!(
            change(&mut r, &mut tree, "src/a.rs", edit),
            Update::Items,
            "{edit}"
        );
        assert_eq!(
            change(&mut r, &mut tree, "src/a.rs", a),
            Update::Items,
            "{a}"
        );
    }
}

#[test]
fn typescript_edits_are_applied_in_place_or_by_item_and_match_a_fresh_build() {
    let mut tree = files(&[
        ("web/package.json", "{ \"name\": \"web\" }"),
        ("web/src/a.ts", "export function f() { return 1; }\n"),
        ("web/src/b.js", "export class B { m() {} }\n"),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    let body = "export function f() {\n  // more\n  return 2;\n}\n";
    assert_eq!(
        change(&mut r, &mut tree, "web/src/a.ts", body),
        Update::InPlace
    );
    let items = "export function f() { return 2; }\nexport function g() {}\n";
    assert_eq!(
        change(&mut r, &mut tree, "web/src/a.ts", items),
        Update::Items
    );
    // Text that would be a `mod` item in Rust does not change a TypeScript
    // file's module.
    let modlike = "export class B { m() {} }\n// mod x;\n";
    assert_eq!(
        change(&mut r, &mut tree, "web/src/b.js", modlike),
        Update::InPlace
    );
    // A new file is a new module.
    change(
        &mut r,
        &mut tree,
        "web/src/c.tsx",
        "export const c = () => 1;\n",
    );
    delete(&mut r, &mut tree, "web/src/c.tsx");
}

#[test]
fn a_js_file_a_rust_module_reaches_is_rusts_and_gives_no_javascript_facts() {
    let mut tree = files(&[
        ("Cargo.toml", "[package]\nname = \"c\"\n"),
        (
            "src/lib.rs",
            "#[path = \"gen.js\"]\nmod gen;\npub fn top() {}\n",
        ),
        (
            "src/gen.js",
            "export function a() { b(); }\nfunction b() {}\n",
        ),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    assert!(
        !r.model()
            .nodes()
            .any(|n| String::from(n.id.clone()).contains("gen/")),
        "no JavaScript items in a Rust module"
    );
    assert!(calls(&r).is_empty(), "{:?}", calls(&r));
    // Its `mod` items are the Rust module tree's, so an edit that adds one
    // must match a fresh build.
    change(
        &mut r,
        &mut tree,
        "src/gen.js",
        "mod inner;\nexport function a() {}\n",
    );
    change(
        &mut r,
        &mut tree,
        "src/gen.js",
        "// \u{ff}\nexport function a() {}\n",
    );
}

#[test]
fn typescript_item_edits_update_the_edges_of_files_that_import_them() {
    let mut tree = files(&[
        (
            "package.json",
            r#"{ "private": true, "workspaces": ["p/*"] }"#,
        ),
        ("p/core/package.json", r#"{ "name": "core" }"#),
        (
            "p/core/src/a.ts",
            "export function f() {}\nexport class Base {}\n",
        ),
        (
            "p/web/package.json",
            r#"{ "name": "web", "dependencies": { "core": "workspace:*" } }"#,
        ),
        (
            "p/web/src/b.ts",
            "import { f, Base } from \"core/src/a\";\nimport * as c from \"./c\";\nexport class K extends Base {}\nexport function g() { f(); c.h(); }\n",
        ),
        ("p/web/src/c.ts", "export function h() {}\n"),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    assert_eq!(calls(&r).len(), 6, "{:?}", calls(&r));
    // Removing what another package imports: its edges change by item.
    let how = change(
        &mut r,
        &mut tree,
        "p/core/src/a.ts",
        "export class Base {}\n",
    );
    assert_eq!(how, Update::Items);
    // And back, renamed class too.
    change(
        &mut r,
        &mut tree,
        "p/core/src/a.ts",
        "export function f() {}\nexport class Other {}\n",
    );
    change(
        &mut r,
        &mut tree,
        "p/core/src/a.ts",
        "export function f() {}\nexport class Base {}\n",
    );
    // The namespace import's target gains and loses a function.
    change(
        &mut r,
        &mut tree,
        "p/web/src/c.ts",
        "export function x() {}\n",
    );
    change(
        &mut r,
        &mut tree,
        "p/web/src/c.ts",
        "export function h() {}\n",
    );
    // The importing file's body and imports change.
    change(
        &mut r,
        &mut tree,
        "p/web/src/b.ts",
        "import { f } from \"core/src/a\";\nexport function g() { f(); }\n",
    );
}

#[test]
fn csharp_edits_match_a_fresh_build() {
    let mut tree = files(&[
        (
            "App/App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>",
        ),
        (
            "App/A.cs",
            "namespace App.Core;\npublic class A { public void F() {} }\n",
        ),
        (
            "App/B.cs",
            "namespace App.Core { public class B { void G() { } } }\n",
        ),
        (
            "App/P1.cs",
            "namespace App;\npublic partial class P { void X() {} }\n",
        ),
        (
            "App/P2.cs",
            "namespace App;\npublic partial class P { void Y() {} }\n",
        ),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    // A body edit, then a new member: in place, then by item.
    let body = "namespace App.Core;\npublic class A {\n  public void F() { int x = 1; }\n}\n";
    assert_eq!(change(&mut r, &mut tree, "App/A.cs", body), Update::InPlace);
    let item = "namespace App.Core;\npublic class A { public void F() {} public void H() {} }\n";
    assert_eq!(change(&mut r, &mut tree, "App/A.cs", item), Update::Items);
    // A new namespace changes the module tree.
    let moved = "namespace App.Other;\npublic class A { public void F() {} }\n";
    assert_eq!(
        change(&mut r, &mut tree, "App/A.cs", moved),
        Update::Rebuilt
    );
    // Either part of a partial type rebuilds, and matches.
    change(
        &mut r,
        &mut tree,
        "App/P2.cs",
        "namespace App;\npublic partial class P { void Y() { int a = 1; } }\n",
    );
    change(
        &mut r,
        &mut tree,
        "App/P1.cs",
        "namespace App;\npublic partial class P { void X() {} void Z() {} }\n",
    );
    change(
        &mut r,
        &mut tree,
        "App/P2.cs",
        "namespace App;\npublic class Q { }\n",
    );
}

#[test]
fn a_type_named_as_a_partial_one_elsewhere_matches_a_fresh_build() {
    let mut tree = files(&[
        (
            "App/App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>",
        ),
        ("App/A.cs", "namespace N;\npublic class A { }\n"),
        (
            "App/B.cs",
            "namespace N;\npublic partial class P { void X() {} }\n",
        ),
        (
            "App/C.cs",
            "namespace N;\npublic partial class P { void Y() {} }\n",
        ),
        ("Cargo.toml", "[package]\nname = \"r\"\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    // A generic type of the same name, in a file with no partial type.
    let generic = "namespace N;\npublic class A { }\npublic class P<T> { void Z() {} }\n";
    change(&mut r, &mut tree, "App/A.cs", generic);
    change(
        &mut r,
        &mut tree,
        "src/lib.rs",
        "pub fn f() { let _ = 1; }\n",
    );
    change(
        &mut r,
        &mut tree,
        "App/A.cs",
        "namespace N;\npublic class A { }\n",
    );
    change(&mut r, &mut tree, "src/lib.rs", "pub fn f() {}\n");
}
