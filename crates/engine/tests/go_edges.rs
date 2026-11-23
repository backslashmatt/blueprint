//! Go edges (#166, DR 0015): what resolves, and what must not be invented
//! (§2.3). Regressions from the adversarial review.

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

fn deep(entries: &[(&str, &str)]) -> (Model, String) {
    let tree = files(entries);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let (m, d) = analyse_deep(&paths, &|p| tree.get(p).cloned());
    let s = render(&m, &d);
    (m, s)
}

fn edge_list(entries: &[(&str, &str)]) -> Vec<String> {
    let (m, _) = deep(entries);
    m.edges()
        .map(|e| {
            format!(
                "{} -> {} {:?} {:?}",
                e.key.from, e.key.to, e.key.kind, e.confidence
            )
        })
        .collect()
}

const GOMOD: &str = "module example.com/app\n\ngo 1.22\n";

// D1: stdlib `log` and a local package in dir `log` whose clause is
// `applog`: Go binds `log` to the stdlib, `applog` to the local one.
#[test]
fn d1_unresolved_import_name_is_bound_to_local_dir_name() {
    let e = edge_list(&[
        ("go.mod", GOMOD),
        (
            "internal/log/log.go",
            "package applog\n\ntype Logger struct{}\n\nfunc Fatal(v ...any) {}\n",
        ),
        (
            "cmd/main.go",
            "package main\n\nimport (\n\t\"log\"\n\t\"example.com/app/internal/log\"\n)\n\ntype L struct{ log.Logger }\n\nfunc main() { log.Fatal(\"x\") }\n",
        ),
    ]);
    let bad: Vec<&String> = e
        .iter()
        .filter(|x| x.contains("Calls") || x.contains("Extends"))
        .filter(|x| x.contains("L4:") || x.contains("L3:"))
        .collect();
    assert!(bad.is_empty(), "invented: {bad:#?}\nall: {e:#?}");
    // The local package is reached by its clause name.
    let e = edge_list(&[
        ("go.mod", GOMOD),
        (
            "internal/log/log.go",
            "package applog\n\nfunc Fatal(v ...any) {}\n",
        ),
        (
            "cmd/main.go",
            "package main\n\nimport (\n\t\"log\"\n\t\"example.com/app/internal/log\"\n)\n\nfunc main() { log.Println(); applog.Fatal() }\n",
        ),
    ]);
    assert!(
        e.iter()
            .any(|x| x.contains("(functions)/main -> L4:go:#internal/log/(functions)/Fatal Calls")),
        "{e:#?}"
    );
}

// D2: a package-level var named like the imported dir, whose clause
// differs: `log.Info()` is a method on the var.
#[test]
fn d2_package_var_named_like_import_dir() {
    let e = edge_list(&[
        ("go.mod", GOMOD),
        (
            "log/log.go",
            "package logging\n\ntype Logger struct{}\n\nfunc (l *Logger) Info() {}\nfunc New() *Logger { return nil }\nfunc Info() {}\n",
        ),
        (
            "svc/svc.go",
            "package svc\n\nimport \"example.com/app/log\"\n\nvar log = logging.New()\n\nfunc Run() { log.Info() }\n",
        ),
    ]);
    assert!(
        !e.iter()
            .any(|x| x.contains("svc/(functions)/Run -> L4:go:#log/(functions)/Info")),
        "{e:#?}"
    );
}

// D3: a dot import does not bring unexported names; `min` is the builtin.
#[test]
fn d3_dot_import_unexported_shadows_builtin() {
    let e = edge_list(&[
        ("go.mod", GOMOD),
        (
            "util/util.go",
            "package util\n\nfunc min(a, b int) int { return a }\nfunc Exported() {}\n",
        ),
        (
            "p/p.go",
            "package p\n\nimport . \"example.com/app/util\"\n\nfunc F() int { Exported(); return min(1, 2) }\n",
        ),
    ]);
    assert!(
        e.iter()
            .any(|x| x.contains("p/(functions)/F -> L4:go:#util/(functions)/Exported Calls")),
        "{e:#?}"
    );
    assert!(
        !e.iter()
            .any(|x| x.contains("-> L4:go:#util/(functions)/min")),
        "{e:#?}"
    );
}

// D4: a func-typed field at depth 0 shadows the embedded type's method.

// D5: an import aliased `self` is read as the receiver.
#[test]
fn d5_import_aliased_self() {
    let e = edge_list(&[
        ("go.mod", GOMOD),
        ("x/x.go", "package x\n\nfunc Do() {}\n"),
        (
            "s/s.go",
            "package s\n\nimport self \"example.com/app/x\"\n\ntype T struct{}\n\nfunc (T) Do() {}\n\nfunc (t T) Go() { self.Do() }\n",
        ),
    ]);
    assert!(
        !e.iter().any(|x| x.contains("s/T/Go -> L4:go:#s/T/Do")),
        "{e:#?}"
    );
}

// Root package: edges into a root package with no items, and roll-up.
#[test]
fn root_package_edges_and_roll_up() {
    let (m, s) = deep(&[
        ("lib/go.mod", "module example.com/lib/v2\n"),
        ("lib/lib.go", "package lib\n\nvar X = 1\n"),
        ("lib/b/b.go", "package b\n\nfunc B() {}\n"),
        (
            "app/go.mod",
            "module example.com/app\n\nrequire example.com/lib/v2 v2.0.0\n\nreplace example.com/lib/v2 => ../lib\n",
        ),
        (
            "app/main.go",
            "package main\n\nimport (\n\t\"example.com/lib/v2\"\n\t\"example.com/lib/v2/b\"\n)\n\nvar _ = lib.X\n\nfunc main() { b.B() }\n",
        ),
    ]);
    println!("{s}");
    let ids: Vec<String> = m.nodes().map(|n| String::from(n.id.clone())).collect();
    assert!(ids.contains(&"L2:go:lib#".to_owned()), "{s}");
    let root = m
        .nodes()
        .find(|n| String::from(n.id.clone()) == "L2:go:lib#")
        .unwrap();
    assert_eq!(root.source.to_string(), "lib/lib.go", "{s}");
    assert!(s.contains("L2:go:app# -> L2:go:lib#"), "{s}");
}

// A missed edge: go.work workspace modules import each other without a
// require.

// Module path prefix, nested modules, self-require.
#[test]
fn module_paths() {
    let e = edge_list(&[
        ("a/go.mod", "module example.com/a\n"),
        ("a/x/x.go", "package x\n\nfunc X() {}\n"),
        ("ab/go.mod", "module example.com/ab\n"),
        ("ab/x/x.go", "package x\n\nfunc X() {}\n"),
        (
            "app/go.mod",
            "module example.com/app\n\nrequire (\n\texample.com/a v0\n\texample.com/ab v0\n\texample.com/app v0\n\texample.com/app/n v0\n)\n",
        ),
        ("app/n/go.mod", "module example.com/app/n\n"),
        ("app/n/sub/s.go", "package sub\n\nfunc S() {}\n"),
        (
            "app/main.go",
            "package main\n\nimport (\n\tax \"example.com/a/x\"\n\tabx \"example.com/ab/x\"\n\t\"example.com/app/n/sub\"\n)\n\nfunc main() { ax.X(); abx.X(); sub.S() }\n",
        ),
    ]);
    let has = |s: &str| e.iter().any(|x| x.contains(s));
    assert!(has("(functions)/main -> L4:go:a#x/(functions)/X"), "{e:#?}");
    assert!(
        has("(functions)/main -> L4:go:ab#x/(functions)/X"),
        "{e:#?}"
    );
    assert!(
        has("(functions)/main -> L4:go:app/n#sub/(functions)/S"),
        "{e:#?}"
    );
}

// Incremental: a body edit adding a call through an import; go.mod edits.
#[test]
fn incremental() {
    let mut tree = files(&[
        ("lib/go.mod", "module example.com/lib\n"),
        ("lib/lib.go", "package lib\n\nfunc Root() {}\n"),
        (
            "lib/u/u.go",
            "package u\n\ntype B struct{}\n\nfunc (B) M() {}\nfunc U() {}\n",
        ),
        ("app/go.mod", "module example.com/app\n"),
        (
            "app/main.go",
            "package main\n\nimport (\n\t\"example.com/lib\"\n\t\"example.com/lib/u\"\n)\n\ntype T struct{ u.B }\n\nfunc (t T) Go() {}\n\nfunc main() {}\n",
        ),
        ("app/s/s.go", "package s\n\nfunc S() {}\n"),
    ]);
    let mut r = build(&tree);
    // go.mod: now requires lib.
    change(
        &mut r,
        &mut tree,
        "app/go.mod",
        "module example.com/app\n\nrequire example.com/lib v0\n\nreplace example.com/lib => ../lib\n",
    );
    let how = change(
        &mut r,
        &mut tree,
        "app/main.go",
        "package main\n\nimport (\n\t\"example.com/lib\"\n\t\"example.com/lib/u\"\n)\n\ntype T struct{ u.B }\n\nfunc (t T) Go() { t.M(); u.B.M(t.B) }\n\nfunc main() { lib.Root(); u.U() }\n",
    );
    assert_eq!(how, Update::InPlace);
    let how = change(
        &mut r,
        &mut tree,
        "app/main.go",
        "package main\n\nimport (\n\t\"example.com/lib\"\n\t\"example.com/lib/u\"\n)\n\ntype T struct{ u.B }\n\nfunc (t T) Go() {}\n\nfunc main() { u.U() }\n",
    );
    assert_eq!(how, Update::InPlace);
    // Module path change of lib: edges go.
    change(
        &mut r,
        &mut tree,
        "lib/go.mod",
        "module example.com/other\n",
    );
    change(&mut r, &mut tree, "lib/go.mod", "module example.com/lib\n");
    change(&mut r, &mut tree, "app/go.mod", "module example.com/app\n");
}

// L1 rule 3 (inferred: same module path, no replace): L2 edge `exact`?
#[test]
fn c1_inferred_l1_gives_exact_l2() {
    let e = edge_list(&[
        ("lib/go.mod", "module example.com/lib\n"),
        ("lib/u/u.go", "package u\n\nfunc U() {}\n"),
        (
            "app/go.mod",
            "module example.com/app\n\nrequire example.com/lib v1.5.0\n",
        ),
        (
            "app/main.go",
            "package main\n\nimport \"example.com/lib/u\"\n\nfunc main() { u.U() }\n",
        ),
    ]);
    println!("{e:#?}");
    assert!(
        !e.iter()
            .any(|x| x.contains("L2:go:app# -> L2:go:lib#u DependsOn Exact")),
        "{e:#?}"
    );
}

// Imports of packages in testdata / vendor / _dir.
#[test]
fn c2_ignored_dirs_are_not_targets() {
    let e = edge_list(&[
        ("go.mod", GOMOD),
        ("testdata/t/t.go", "package t\n\nfunc T() {}\n"),
        ("_x/x.go", "package x\n\nfunc X() {}\n"),
        (
            "main.go",
            "package main\n\nimport (\n\t\"example.com/app/testdata/t\"\n\t\"example.com/app/_x\"\n)\n\nfunc main() { t.T(); x.X() }\n",
        ),
    ]);
    println!("{e:#?}");
    assert!(e.is_empty(), "{e:#?}");
}

// Embedding: pointer, generic, method in another file, two embedded types.
#[test]
fn c3_embedding_shapes() {
    let e = edge_list(&[
        ("go.mod", GOMOD),
        (
            "s/base.go",
            "package s\n\ntype Base struct{}\ntype G[T any] struct{}\ntype A struct{}\ntype B struct{}\n",
        ),
        (
            "s/m.go",
            "package s\n\nfunc (*Base) Run() {}\nfunc (G[T]) Gen() {}\nfunc (A) X() {}\nfunc (B) X() {}\n",
        ),
        (
            "s/t.go",
            "package s\n\ntype T struct {\n\t*Base\n\tG[int]\n}\n\nfunc (t *T) Go() { t.Run(); t.Gen() }\n\ntype U struct{ A; B }\n\nfunc (u U) Go() { u.X() }\n",
        ),
    ]);
    let has = |s: &str| e.iter().any(|x| x.contains(s));
    assert!(has("L4:go:#s/T/Go -> L4:go:#s/Base/Run"), "{e:#?}");
    assert!(has("L4:go:#s/T/Go -> L4:go:#s/G/Gen"), "{e:#?}");
    assert!(!has("L4:go:#s/U/Go ->"), "{e:#?}");
}

#[test]
fn c1b_l1_alone_is_inferred() {
    let e = edge_list(&[
        ("lib/go.mod", "module example.com/lib\n"),
        ("lib/u/u.go", "package u\n\nfunc U() {}\n"),
        (
            "app/go.mod",
            "module example.com/app\n\nrequire example.com/lib v1.5.0\n",
        ),
        ("app/main.go", "package main\n\nfunc main() {}\n"),
    ]);
    assert_eq!(
        e,
        vec!["L1:go:app# -> L1:go:lib# DependsOn Inferred".to_owned()]
    );
}
