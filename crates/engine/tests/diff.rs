//! The architectural diff (DR 0008 decision 6) on small hand-written
//! changes. The full fixture suite of M2's exit test is #65.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::analyse_deep;
use blueprint_engine::diff::{Diff, Finding, NodeChange, diff};
use blueprint_model::{Model, RepoPath};

use common::files;

fn model(entries: &[(&str, &str)]) -> Model {
    let files = files(entries);
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    analyse_deep(&paths, &|p| files.get(p).cloned()).0
}

const MANIFEST: (&str, &str) = ("Cargo.toml", "[package]\nname = \"app\"\n");

/// The diff between a crate with `base` as its sources and with `head`.
fn crate_diff(base: &[(&str, &str)], head: &[(&str, &str)]) -> Diff {
    let with = |src: &[(&str, &str)]| {
        let mut v = vec![MANIFEST];
        v.extend_from_slice(src);
        model(&v)
    };
    diff(&with(base), &with(head))
}

fn changes(d: &Diff) -> Vec<String> {
    d.nodes
        .iter()
        .map(|n| match n {
            NodeChange::Added { id } => format!("added {id}"),
            NodeChange::Removed { id } => format!("removed {id}"),
            NodeChange::Moved { from, to } => format!("moved {from} -> {to}"),
            NodeChange::Renamed { from, to } => format!("renamed {from} -> {to}"),
            NodeChange::Surface { id } => format!("surface {id}"),
            NodeChange::Contained { id } => format!("contained {id}"),
            NodeChange::Changed { id } => format!("changed {id}"),
        })
        .collect()
}

const LIB: &str = "src/lib.rs";

#[test]
fn identical_models_give_an_empty_diff() {
    let src = [
        (LIB, "mod a;\npub fn f() { a::g(); }\n"),
        ("src/a.rs", "pub fn g() {}\n"),
    ];
    let d = crate_diff(&src, &src);
    assert!(d.is_empty(), "{d:#?}");
    assert!(d.findings.is_empty());
}

#[test]
fn a_body_only_change_is_contained() {
    let d = crate_diff(
        &[(LIB, "pub fn f() { let x = 1; }\n")],
        &[(LIB, "pub fn f() { let x = 2; }\n")],
    );
    assert_eq!(changes(&d), ["contained L4:cargo:#(functions)/f"]);
}

#[test]
fn moving_lines_alone_is_no_change() {
    let d = crate_diff(
        &[(LIB, "pub fn f() {}\n")],
        &[(LIB, "\n\n// moved down\npub fn f() {}\n")],
    );
    assert_eq!(changes(&d), Vec::<String>::new());
}

#[test]
fn a_public_signature_change_is_surface_and_a_private_one_is_changed() {
    let d = crate_diff(
        &[(LIB, "pub fn f() {}\nfn g() {}\n")],
        &[(LIB, "pub fn f(x: u8) {}\nfn g(x: u8) {}\n")],
    );
    assert_eq!(
        changes(&d),
        [
            "surface L4:cargo:#(functions)/f",
            "changed L4:cargo:#(functions)/g",
        ]
    );
    // Making something public is a surface change too.
    let d = crate_diff(&[(LIB, "fn g() {}\n")], &[(LIB, "pub fn g() {}\n")]);
    assert_eq!(changes(&d), ["surface L4:cargo:#(functions)/g"]);
}

#[test]
fn a_body_change_that_adds_a_call_is_changed_not_contained() {
    let d = crate_diff(
        &[(LIB, "pub fn f() {}\npub fn g() {}\n")],
        &[(LIB, "pub fn f() { g(); }\npub fn g() {}\n")],
    );
    assert!(
        changes(&d).contains(&"changed L4:cargo:#(functions)/f".to_owned()),
        "{d:#?}"
    );
    assert_eq!(d.added_edges.len(), 1, "{d:#?}");
}

#[test]
fn the_same_body_in_another_module_is_moved_and_under_another_name_renamed() {
    let d = crate_diff(
        &[
            (LIB, "mod a;\nmod b;\n"),
            ("src/a.rs", "pub fn f() { let x = 1; }\n"),
            ("src/b.rs", ""),
        ],
        &[
            (LIB, "mod a;\nmod b;\n"),
            ("src/a.rs", ""),
            ("src/b.rs", "pub fn f() { let x = 1; }\n"),
        ],
    );
    // The modules' groups of free functions come and go with it.
    assert_eq!(
        changes(&d),
        [
            "added L3:cargo:#b/(functions)",
            "removed L3:cargo:#a/(functions)",
            "moved L4:cargo:#a/(functions)/f -> L4:cargo:#b/(functions)/f",
        ]
    );
    let d = crate_diff(
        &[(LIB, "pub fn old() { let x = 1; }\n")],
        &[(LIB, "pub fn new() { let x = 1; }\n")],
    );
    assert_eq!(
        changes(&d),
        ["renamed L4:cargo:#(functions)/old -> L4:cargo:#(functions)/new"]
    );
}

#[test]
fn a_new_use_across_modules_is_a_new_dependency() {
    let d = crate_diff(
        &[
            (LIB, "mod a;\nmod b;\n"),
            ("src/a.rs", "pub fn f() {}\n"),
            ("src/b.rs", ""),
        ],
        &[
            (LIB, "mod a;\nmod b;\n"),
            ("src/a.rs", "pub fn f() {}\n"),
            ("src/b.rs", "use crate::a::f;\n"),
        ],
    );
    assert!(
        d.findings
            .iter()
            .any(|f| matches!(f, Finding::NewDependency { edge }
            if edge.from.to_string() == "L2:cargo:#b" && edge.to.to_string() == "L2:cargo:#a")),
        "{:#?}",
        d.findings
    );
}

#[test]
fn a_use_that_closes_a_loop_is_a_new_cycle() {
    let base = [
        (LIB, "mod a;\nmod b;\n"),
        ("src/a.rs", "use crate::b::g;\npub fn f() {}\n"),
        ("src/b.rs", "pub fn g() {}\n"),
    ];
    let head = [
        (LIB, "mod a;\nmod b;\n"),
        ("src/a.rs", "use crate::b::g;\npub fn f() {}\n"),
        ("src/b.rs", "use crate::a::f;\npub fn g() {}\n"),
    ];
    let d = crate_diff(&base, &head);
    let cycles: Vec<&Finding> = d
        .findings
        .iter()
        .filter(|f| matches!(f, Finding::NewCycle { .. }))
        .collect();
    assert_eq!(cycles.len(), 1, "{:#?}", d.findings);
    let Finding::NewCycle {
        level,
        nodes,
        new_edges,
    } = cycles[0]
    else {
        unreachable!()
    };
    assert_eq!(*level, 2);
    let nodes: Vec<String> = nodes.iter().map(ToString::to_string).collect();
    assert_eq!(nodes, ["L2:cargo:#a", "L2:cargo:#b"]);
    assert_eq!(new_edges.len(), 1);
    // The cycle that already existed is not new.
    assert!(crate_diff(&head, &head).findings.is_empty());
}

/// The diff between an npm package with `base` as its sources and with `head`.
fn package_diff(base: &[(&str, &str)], head: &[(&str, &str)]) -> Diff {
    let with = |src: &[(&str, &str)]| {
        let mut v = vec![("package.json", "{ \"name\": \"web\" }")];
        v.extend_from_slice(src);
        model(&v)
    };
    diff(&with(base), &with(head))
}

#[test]
fn typescript_changes_are_told_apart_as_rust_ones_are() {
    const A: &str = "src/a.ts";
    let d = package_diff(
        &[(A, "export function f(x: number) { return x + 1; }\n")],
        &[(A, "export function f(x: number) {\n  return x + 2;\n}\n")],
    );
    assert_eq!(changes(&d), ["contained L4:npm:#src/a/(functions)/f"]);
    let d = package_diff(
        &[(A, "export function f(x: number) {}\nfunction g() {}\n")],
        &[(A, "export function f(x: string) {}\nfunction g(y) {}\n")],
    );
    assert_eq!(
        changes(&d),
        [
            "surface L4:npm:#src/a/(functions)/f",
            "changed L4:npm:#src/a/(functions)/g",
        ]
    );
    // A class's method moved to another file is a move.
    let d = package_diff(
        &[
            (A, "export class C { m() { return 1; } }\n"),
            ("src/b.ts", "export class D {}\n"),
        ],
        &[
            (A, "export class C {}\n"),
            ("src/b.ts", "export class D { m() { return 1; } }\n"),
        ],
    );
    assert!(
        changes(&d).contains(&"moved L4:npm:#src/a/C/m -> L4:npm:#src/b/D/m".to_owned()),
        "{:#?}",
        changes(&d)
    );
}

#[test]
fn a_new_typescript_import_is_a_new_dependency_and_one_closing_a_loop_a_new_cycle() {
    let base = [
        (
            "src/a.ts",
            "import { g } from \"./b\";\nexport function f() {}\n",
        ),
        ("src/b.ts", "export function g() {}\n"),
    ];
    let head = [
        (
            "src/a.ts",
            "import { g } from \"./b\";\nexport function f() {}\n",
        ),
        (
            "src/b.ts",
            "import { f } from \"./a\";\nexport function g() {}\n",
        ),
    ];
    let d = package_diff(&base, &head);
    assert!(
        d.findings
            .iter()
            .any(|f| matches!(f, Finding::NewDependency { edge }
            if edge.from.to_string() == "L2:npm:#src/b" && edge.to.to_string() == "L2:npm:#src/a")),
        "{:#?}",
        d.findings
    );
    let cycles: Vec<&Finding> = d
        .findings
        .iter()
        .filter(|f| matches!(f, Finding::NewCycle { .. }))
        .collect();
    assert_eq!(cycles.len(), 1, "{:#?}", d.findings);
    assert!(package_diff(&head, &head).findings.is_empty());
    // An import of a package from outside the repository is no finding.
    let ext = [(
        "src/b.ts",
        "import React from \"react\";\nexport function g() {}\n",
    )];
    let d = package_diff(&[("src/b.ts", "export function g() {}\n")], &ext);
    assert!(d.findings.is_empty(), "{:#?}", d.findings);
}

/// The diff between a .NET project with `base` as its sources and `head`.
fn project_diff(base: &[(&str, &str)], head: &[(&str, &str)]) -> Diff {
    let with = |src: &[(&str, &str)]| {
        let mut v = vec![(
            "App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>",
        )];
        v.extend_from_slice(src);
        model(&v)
    };
    diff(&with(base), &with(head))
}

#[test]
fn csharp_changes_are_told_apart_and_partial_parts_join() {
    const A: &str = "Core/A.cs";
    let d = project_diff(
        &[(
            A,
            "namespace App;\npublic class C { public int F(int x) { return x + 1; } }\n",
        )],
        &[(
            A,
            "namespace App;\npublic class C {\n  public int F(int x) { return x + 2; }\n}\n",
        )],
    );
    // The class's methods are its body, so it is contained too.
    assert_eq!(
        changes(&d),
        ["contained L3:dotnet:#App/C", "contained L4:dotnet:#App/C/F"]
    );
    let d = project_diff(
        &[(
            A,
            "namespace App;\npublic class C { public void F(int x) {} void G() {} }\n",
        )],
        &[(
            A,
            "namespace App;\npublic class C { public void F(long x) {} void G(int y) {} }\n",
        )],
    );
    let got = changes(&d);
    assert!(
        got.contains(&"surface L4:dotnet:#App/C/F".to_owned()),
        "{got:#?}"
    );
    assert!(
        got.contains(&"changed L4:dotnet:#App/C/G".to_owned()),
        "{got:#?}"
    );
    // A body change in either part of a partial class changes the one node.
    const B: &str = "Core/A.More.cs";
    let base = [
        (
            A,
            "namespace App;\npublic partial class P { void F() {} }\n",
        ),
        (
            B,
            "namespace App;\npublic partial class P { void G() { int a = 1; } }\n",
        ),
    ];
    let head = [
        (
            A,
            "namespace App;\npublic partial class P { void F() {} }\n",
        ),
        (
            B,
            "namespace App;\npublic partial class P { void G() { int a = 2; } }\n",
        ),
    ];
    let d = project_diff(&base, &head);
    assert!(
        changes(&d).contains(&"contained L4:dotnet:#App/P/G".to_owned()),
        "{:#?}",
        changes(&d)
    );
    assert!(
        !changes(&d).iter().any(|c| c.contains("P~2")),
        "{:#?}",
        changes(&d)
    );
}
