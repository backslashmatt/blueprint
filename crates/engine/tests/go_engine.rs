//! Go in the engine (#164, DR 0015): packages across files, and what the
//! go tool ignores. Each one-file change must match a fresh build.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::analyse_deep;
use blueprint_engine::resident::{Resident, Update};
use blueprint_model::{Model, RepoPath};

use common::{Files, files, render};

fn change(r: &mut Resident, tree: &mut Files, path: &str, text: &str) -> Update {
    let path = RepoPath::parse(path).unwrap();
    tree.insert(path.clone(), text.as_bytes().to_vec());
    check(r, tree, &path)
}

fn delete(r: &mut Resident, tree: &mut Files, path: &str) -> Update {
    let path = RepoPath::parse(path).unwrap();
    tree.remove(&path);
    check(r, tree, &path)
}

fn check(r: &mut Resident, tree: &Files, path: &RepoPath) -> Update {
    let how = r.update(path, &|p| tree.get(p).cloned());
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let (fresh, diags) = analyse_deep(&paths, &|p| tree.get(p).cloned());
    assert_eq!(
        render(r.model(), r.diagnostics()),
        render(&fresh, &diags),
        "after changing {path} ({how:?})"
    );
    assert_eq!(*r.model(), fresh, "after changing {path} ({how:?})");
    how
}

fn build(tree: &Files) -> Resident {
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    Resident::build(&paths, &|p| tree.get(p).cloned())
}

fn deep(tree: &Files) -> (Model, String) {
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let (m, d) = analyse_deep(&paths, &|p| tree.get(p).cloned());
    let s = render(&m, &d);
    (m, s)
}

const GOMOD: &str = "module example.com/app\n\ngo 1.22\n";

#[test]
fn methods_follow_their_type_across_files() {
    let mut tree = files(&[
        ("go.mod", GOMOD),
        ("s/a.go", "package s\n\ntype T struct{}\n"),
        (
            "s/b.go",
            "package s\n\nfunc (t *T) M() {}\nfunc (t T) N() int { return 1 }\n",
        ),
        ("s/c.go", "package s\n\nfunc init() {}\nfunc M() {}\n"),
        ("s/d.go", "package s\n\nfunc init() {}\n"),
    ]);
    let mut r = build(&tree);
    // Type moves to another file.
    change(&mut r, &mut tree, "s/a.go", "package s\n");
    change(
        &mut r,
        &mut tree,
        "s/d.go",
        "package s\n\ntype T struct{}\n\nfunc init() {}\n",
    );
    change(
        &mut r,
        &mut tree,
        "s/a.go",
        "package s\n\ntype T struct{ x int }\n",
    );
    // Body edits in place.
    assert_eq!(
        change(
            &mut r,
            &mut tree,
            "s/b.go",
            "package s\n\nfunc (t *T) M() { println() }\nfunc (t T) N() int { return 2 }\n"
        ),
        Update::InPlace
    );
    assert_eq!(
        change(
            &mut r,
            &mut tree,
            "s/c.go",
            "package s\n\nfunc init() { println() }\nfunc M() {}\n"
        ),
        Update::InPlace
    );
    assert_eq!(
        change(
            &mut r,
            &mut tree,
            "s/d.go",
            "package s\n\ntype T struct{}\n\nfunc init() { println(1) }\n"
        ),
        Update::InPlace
    );
    // Duplicate type deleted.
    delete(&mut r, &mut tree, "s/a.go");
    delete(&mut r, &mut tree, "s/d.go");
    // impl T group, then the type comes back.
    change(&mut r, &mut tree, "s/z.go", "package s\n\ntype T int\n");
    change(
        &mut r,
        &mut tree,
        "s/b.go",
        "package s\n\nfunc (t *T) M() { println(3) }\nfunc (t T) N() int { return 2 }\n",
    );
}

#[test]
fn build_tags_x_test_root_files_and_nested_modules() {
    let mut tree = files(&[
        ("go.mod", GOMOD),
        ("main.go", "package main\n\nfunc main() {}\n"),
        ("util.go", "package main\n\nfunc util() {}\n"),
        (
            "p/p_linux.go",
            "//go:build linux\n\npackage p\n\nfunc Open() {}\n",
        ),
        (
            "p/p_windows.go",
            "//go:build windows\n\npackage p\n\nfunc Open() {}\n",
        ),
        ("p/p_test.go", "package p_test\n\nfunc TestX() {}\n"),
        ("q/only_test.go", "package q\n\nfunc TestQ() {}\n"),
        ("internal/i.go", "package internal\n\nfunc I() {}\n"),
        ("n/sub/s.go", "package sub\n\nfunc S() {}\n"),
        ("n/n.go", "package n\n\nfunc N() {}\n"),
        ("_x.go", "package main\n\nfunc main() {}\n"),
    ]);
    let mut r = build(&tree);
    assert_eq!(
        change(
            &mut r,
            &mut tree,
            "p/p_windows.go",
            "//go:build windows\n\npackage p\n\nfunc Open() { println() }\n"
        ),
        Update::InPlace
    );
    assert_eq!(
        change(
            &mut r,
            &mut tree,
            "p/p_linux.go",
            "//go:build linux\n\npackage p\n\nfunc Open() { println(1) }\n"
        ),
        Update::InPlace
    );
    delete(&mut r, &mut tree, "p/p_linux.go");
    change(
        &mut r,
        &mut tree,
        "p/p_linux.go",
        "//go:build linux\n\npackage p\n\nfunc Open() {}\n",
    );
    change(
        &mut r,
        &mut tree,
        "p/p_test.go",
        "package p_test\n\nfunc TestX() {}\nfunc Open() {}\n",
    );
    change(
        &mut r,
        &mut tree,
        "main.go",
        "package main\n\nfunc main() {}\nfunc util() {}\n",
    );
    assert_eq!(
        change(
            &mut r,
            &mut tree,
            "util.go",
            "package main\n\nfunc util() { println() }\n"
        ),
        Update::InPlace
    );
    // Nested module steals files.
    change(
        &mut r,
        &mut tree,
        "n/go.mod",
        "module example.com/n\n\ngo 1.22\n",
    );
    change(
        &mut r,
        &mut tree,
        "n/sub/s.go",
        "package sub\n\nfunc S() { println() }\n",
    );
    delete(&mut r, &mut tree, "n/go.mod");
    change(
        &mut r,
        &mut tree,
        "go.mod",
        "module example.com/app2\n\ngo 1.22\n",
    );
    delete(&mut r, &mut tree, "main.go");
    delete(&mut r, &mut tree, "util.go");
}

#[test]
fn odd_directory_names_and_shared_dirs() {
    let mut tree = files(&[
        ("go.mod", GOMOD),
        ("package.json", "{\"name\": \"web\"}\n"),
        ("index.ts", "export function f() {}\n"),
        ("root.go", "package main\n\nfunc T() {}\ntype U struct{}\n"),
        (
            "(functions)/f.go",
            "package f\n\nfunc G() {}\ntype T struct{}\n",
        ),
        ("impl T/f.go", "package f\n\nfunc G() {}\n"),
        ("T/f.go", "package f\n\nfunc G() {}\n"),
        ("U/x.go", "package u\n\ntype V struct{}\n"),
        ("store/s.go", "package store\n\nfunc S() {}\n"),
        ("store/README.md", "hi\n"),
        ("store/x.rs", "fn main() {}\n"),
        ("store/y.py", "def f(): pass\n"),
        ("vendor/v/v.go", "package v\n"),
        ("crate/Cargo.toml", "[package]\nname = \"c\"\n"),
        ("crate/src/lib.rs", "pub fn a() {}\n"),
        ("crate/src/x.go", "package x\n\nfunc X() {}\n"),
    ]);
    // A package directory named like a group is still its own module.
    let (m, _) = deep(&tree);
    assert!(
        m.nodes().any(|n| n.id.to_string() == "L2:go:#(functions)"),
        "no package module for (functions)/"
    );
    let mut r = build(&tree);
    change(
        &mut r,
        &mut tree,
        "(functions)/f.go",
        "package f\n\nfunc G() {}\nfunc H() {}\n",
    );
    change(
        &mut r,
        &mut tree,
        "crate/src/x.go",
        "package x\n\nfunc X() {}\nfunc Y() {}\n",
    );
    change(&mut r, &mut tree, "store/README.md", "ho\n");
    change(
        &mut r,
        &mut tree,
        "root.go",
        "package main\n\nfunc T() {}\n",
    );
}

fn ids(m: &Model) -> Vec<String> {
    m.nodes().map(|n| String::from(n.id.clone())).collect()
}

/// DEFECT: the go tool ignores files whose names start with `_` or `.`
/// (`_scratch.go`, editor lock files `.#x.go`), but L2 makes them package
/// files: `_scratch.go`'s `main` takes the plain id and the real one becomes
/// `main~2`, and the root package node is sourced from the ignored file.
#[test]
fn files_the_go_tool_ignores_are_not_package_files() {
    let tree = files(&[
        ("go.mod", GOMOD),
        ("main.go", "package main\n\nfunc main() {}\n"),
        ("_scratch.go", "package main\n\nfunc main() {}\n"),
        (".#main.go", "package main\n\nfunc main() {}\n"),
    ]);
    let (m, s) = deep(&tree);
    let main = m
        .nodes()
        .find(|n| String::from(n.id.clone()) == "L4:go:#(functions)/main")
        .unwrap();
    assert_eq!(main.source.to_string(), "main.go", "{s}");
    assert!(!ids(&m).iter().any(|i| i.contains("main~")), "{s}");
}

/// DEFECT: a Go package directory that also holds an npm `package.json`
/// (the common `ui/embed.go` + `//go:embed dist` pattern) is owned by the
/// npm system, the deepest one, so the Go package disappears from the Go
/// module and its `.go` files become unknown npm files.
#[test]
fn a_go_package_beside_a_package_json_stays_in_the_go_module() {
    let tree = files(&[
        ("go.mod", GOMOD),
        ("main.go", "package main\n\nfunc main() {}\n"),
        ("ui/package.json", "{\"name\": \"ui\"}\n"),
        ("ui/src/app.ts", "export function app() {}\n"),
        (
            "ui/embed.go",
            "package ui\n\nimport \"embed\"\n\n//go:embed dist\nvar Dist embed.FS\n\nfunc Handler() {}\n",
        ),
    ]);
    let (m, s) = deep(&tree);
    let ids = ids(&m);
    assert!(ids.contains(&"L2:go:#ui".to_owned()), "{s}");
    assert!(
        ids.contains(&"L4:go:#ui/(functions)/Handler".to_owned()),
        "{s}"
    );
}
