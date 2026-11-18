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
