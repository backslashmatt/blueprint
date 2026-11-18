//! M2's exit test (#65): `bp diff <base> <head>` on hand-written changes.
//! Each case is two commits of a small repository and the exact diff a
//! reviewer should get, worked out from the rules in docs/diff-json.md and
//! DR 0008, not copied from the tool's output. Lines are compared as sets.

#![allow(clippy::unwrap_used, clippy::panic, clippy::too_many_lines)] // test code

use std::path::{Path, PathBuf};
use std::process::Command;

type Files<'a> = &'a [(&'a str, &'a str)];

/// One hand-written change.
struct Case<'a> {
    name: &'a str,
    base: Files<'a>,
    head: Files<'a>,
    expect: &'a [&'a str],
}

fn write_tree(repo: &gix::Repository, files: &[(&str, gix::ObjectId)]) -> gix::ObjectId {
    use gix::objs::tree::{Entry, EntryKind};
    use std::collections::BTreeMap;
    let mut here: BTreeMap<&str, Vec<(&str, gix::ObjectId)>> = BTreeMap::new();
    let mut tree = gix::objs::Tree::empty();
    for (path, id) in files {
        match path.split_once('/') {
            Some((dir, rest)) => here.entry(dir).or_default().push((rest, *id)),
            None => tree.entries.push(Entry {
                mode: EntryKind::Blob.into(),
                filename: (*path).into(),
                oid: *id,
            }),
        }
    }
    for (dir, children) in here {
        tree.entries.push(Entry {
            mode: EntryKind::Tree.into(),
            filename: dir.into(),
            oid: write_tree(repo, &children),
        });
    }
    tree.entries.sort();
    repo.write_object(&tree).unwrap().detach()
}

/// A repository whose `HEAD~1` holds `base` and `HEAD` holds `head`.
fn repo(name: &str, base: Files<'_>, head: Files<'_>) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bp-diff-suite-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = gix::init(&dir).unwrap();
    let sig = gix::actor::SignatureRef {
        name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        time: "1700000000 +0000",
    };
    let mut parent: Option<gix::ObjectId> = None;
    for files in [base, head] {
        let blobs: Vec<(&str, gix::ObjectId)> = files
            .iter()
            .map(|(p, body)| (*p, repo.write_blob(body.as_bytes()).unwrap().detach()))
            .collect();
        let tree = write_tree(&repo, &blobs);
        let id = repo
            .commit_as(sig, sig, "HEAD", "c", tree, parent)
            .unwrap()
            .detach();
        parent = Some(id);
    }
    dir
}

fn bp_diff(dir: &Path) -> Vec<String> {
    let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bp-diff-suite-cache");
    let out = Command::new(env!("CARGO_BIN_EXE_bp"))
        .args(["diff", dir.to_str().unwrap(), "HEAD~1", "HEAD"])
        .env("BLUEPRINT_CACHE_DIR", cache)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        // Change weight has cases of its own below.
        .filter(|l| !l.starts_with("weight\t"))
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

// A crate `app` with modules `a` and `b`.
const MANIFEST: (&str, &str) = ("Cargo.toml", "[package]\nname = \"app\"\n");
const LIB: (&str, &str) = ("src/lib.rs", "mod a;\nmod b;\n");

// A workspace with crates `core` and `app`.
const WS: (&str, &str) = ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n");
const CORE: (&str, &str) = ("core/Cargo.toml", "[package]\nname = \"core\"\n");
const CORE_LIB: (&str, &str) = ("core/src/lib.rs", "pub fn c() { let _ = 5; }\n");
const APP: (&str, &str) = ("app/Cargo.toml", "[package]\nname = \"app\"\n");
const APP_ON_CORE: (&str, &str) = (
    "app/Cargo.toml",
    "[package]\nname = \"app\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
);
const APP_MAIN: (&str, &str) = ("app/src/main.rs", "fn main() { let _ = 6; }\n");

#[test]
fn every_hand_written_change_gives_its_expected_diff() {
    let cases: &[Case<'_>] = &[
        Case {
            name: "use-adds-a-module-dependency",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "pub fn g() {}\n"),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "use crate::a::f;\npub fn g() {}\n"),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:cargo:#b -> L2:cargo:#a\tdepends-on",
                "+edge\tL2:cargo:#b -> L2:cargo:#a\tdepends-on",
            ],
        },
        Case {
            name: "use-removed",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "use crate::a::f;\npub fn g() {}\n"),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "pub fn g() {}\n"),
            ],
            expect: &["-edge\tL2:cargo:#b -> L2:cargo:#a\tdepends-on"],
        },
        Case {
            name: "call-across-modules",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "pub fn g() {}\n"),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "pub fn g() { crate::a::f(); }\n"),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:cargo:#b -> L2:cargo:#a\tcalls",
                "changed\tL4:cargo:#b/(functions)/g",
                "+edge\tL4:cargo:#b/(functions)/g -> L4:cargo:#a/(functions)/f\tcalls",
                "+edge\tL3:cargo:#b/(functions) -> L3:cargo:#a/(functions)\tcalls",
                "+edge\tL2:cargo:#b -> L2:cargo:#a\tcalls",
            ],
        },
        Case {
            name: "call-within-a-module",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\npub fn h() {}\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() { h(); }\npub fn h() {}\n"),
                ("src/b.rs", ""),
            ],
            expect: &[
                "changed\tL4:cargo:#a/(functions)/f",
                "+edge\tL4:cargo:#a/(functions)/f -> L4:cargo:#a/(functions)/h\tcalls",
            ],
        },
        Case {
            name: "call-removed",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() { h(); }\npub fn h() {}\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\npub fn h() {}\n"),
                ("src/b.rs", ""),
            ],
            expect: &[
                "changed\tL4:cargo:#a/(functions)/f",
                "-edge\tL4:cargo:#a/(functions)/f -> L4:cargo:#a/(functions)/h\tcalls",
            ],
        },
        Case {
            name: "use-closes-a-module-cycle",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "use crate::a::f;\npub fn g() {}\n"),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "use crate::b::g;\npub fn f() {}\n"),
                ("src/b.rs", "use crate::a::f;\npub fn g() {}\n"),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:cargo:#a -> L2:cargo:#b\tdepends-on",
                "finding\tnew-cycle\tL2\tL2:cargo:#a, L2:cargo:#b\tnew: L2:cargo:#a -> L2:cargo:#b",
                "+edge\tL2:cargo:#a -> L2:cargo:#b\tdepends-on",
            ],
        },
        Case {
            name: "an-existing-cycle-is-not-new",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "use crate::b::g;\npub fn f() { let _ = 1; }\n"),
                ("src/b.rs", "use crate::a::f;\npub fn g() {}\n"),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "use crate::b::g;\npub fn f() { let _ = 2; }\n"),
                ("src/b.rs", "use crate::a::f;\npub fn g() {}\n"),
            ],
            expect: &["contained\tL4:cargo:#a/(functions)/f"],
        },
        Case {
            name: "function-moved-to-another-module",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\npub fn moved() { let _ = 1; }\n"),
                ("src/b.rs", "pub fn g() {}\n"),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "pub fn g() {}\npub fn moved() { let _ = 1; }\n"),
            ],
            expect: &["moved\tL4:cargo:#a/(functions)/moved -> L4:cargo:#b/(functions)/moved"],
        },
        Case {
            name: "function-renamed",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn old() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn new() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            expect: &["renamed\tL4:cargo:#a/(functions)/old -> L4:cargo:#a/(functions)/new"],
        },
        Case {
            name: "method-moved-with-its-type",
            base: &[
                MANIFEST,
                LIB,
                (
                    "src/a.rs",
                    "pub struct S;\nimpl S {\n    pub fn go(&self) { let _ = 1; }\n}\n",
                ),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", ""),
                (
                    "src/b.rs",
                    "pub struct S;\nimpl S {\n    pub fn go(&self) { let _ = 1; }\n}\n",
                ),
            ],
            expect: &[
                "moved\tL3:cargo:#a/S -> L3:cargo:#b/S",
                "moved\tL4:cargo:#a/S/go -> L4:cargo:#b/S/go",
            ],
        },
        Case {
            name: "struct-with-fields-moved",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub struct P {\n    pub x: u8,\n}\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", ""),
                ("src/b.rs", "pub struct P {\n    pub x: u8,\n}\n"),
            ],
            expect: &["moved\tL3:cargo:#a/P -> L3:cargo:#b/P"],
        },
        Case {
            name: "struct-renamed-is-removed-and-added",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub struct Old {\n    pub x: u8,\n}\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub struct New {\n    pub x: u8,\n}\n"),
                ("src/b.rs", ""),
            ],
            // Without a body, a rename cannot be told from a new type.
            expect: &["added\tL3:cargo:#a/New", "removed\tL3:cargo:#a/Old"],
        },
        Case {
            name: "public-signature-change-is-surface",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f(x: u8) { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            expect: &["surface\tL4:cargo:#a/(functions)/f"],
        },
        Case {
            name: "made-public-is-surface",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "fn h() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn h() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            expect: &["surface\tL4:cargo:#a/(functions)/h"],
        },
        Case {
            name: "made-private-is-surface",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn h() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "fn h() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            expect: &["surface\tL4:cargo:#a/(functions)/h"],
        },
        Case {
            name: "restricted-signature-change-is-surface",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub(crate) fn h() {}\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub(crate) fn h() -> u8 { 0 }\n"),
                ("src/b.rs", ""),
            ],
            expect: &["surface\tL4:cargo:#a/(functions)/h"],
        },
        Case {
            name: "body-only-change-is-contained",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() { let _ = 2; }\n"),
                ("src/b.rs", ""),
            ],
            expect: &["contained\tL4:cargo:#a/(functions)/f"],
        },
        Case {
            name: "private-signature-change-is-changed",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "fn h(x: u8) { let _ = x; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "fn h(x: u16) { let _ = x; }\n"),
                ("src/b.rs", ""),
            ],
            expect: &["changed\tL4:cargo:#a/(functions)/h"],
        },
        Case {
            name: "function-added",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\npub fn added() { let _ = 3; }\n"),
                ("src/b.rs", ""),
            ],
            expect: &["added\tL4:cargo:#a/(functions)/added"],
        },
        Case {
            name: "function-removed",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\npub fn gone() { let _ = 3; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", ""),
            ],
            expect: &["removed\tL4:cargo:#a/(functions)/gone"],
        },
        Case {
            name: "module-added",
            base: &[MANIFEST, LIB, ("src/a.rs", ""), ("src/b.rs", "")],
            head: &[
                MANIFEST,
                ("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
                ("src/a.rs", ""),
                ("src/b.rs", ""),
                ("src/c.rs", "pub fn k() { let _ = 4; }\n"),
            ],
            expect: &[
                "added\tL2:cargo:#c",
                "added\tL3:cargo:#c/(functions)",
                "added\tL4:cargo:#c/(functions)/k",
            ],
        },
        Case {
            name: "module-removed",
            base: &[
                MANIFEST,
                ("src/lib.rs", "mod a;\nmod b;\nmod c;\n"),
                ("src/a.rs", ""),
                ("src/b.rs", ""),
                ("src/c.rs", "pub fn k() { let _ = 4; }\n"),
            ],
            head: &[MANIFEST, LIB, ("src/a.rs", ""), ("src/b.rs", "")],
            expect: &[
                "removed\tL2:cargo:#c",
                "removed\tL3:cargo:#c/(functions)",
                "removed\tL4:cargo:#c/(functions)/k",
            ],
        },
        Case {
            name: "module-renamed-moves-its-functions",
            base: &[
                MANIFEST,
                ("src/lib.rs", "mod a;\n"),
                ("src/a.rs", "pub fn f() { let _ = 1; }\n"),
            ],
            head: &[
                MANIFEST,
                ("src/lib.rs", "mod z;\n"),
                ("src/z.rs", "pub fn f() { let _ = 1; }\n"),
            ],
            expect: &[
                "added\tL2:cargo:#z",
                "added\tL3:cargo:#z/(functions)",
                "removed\tL2:cargo:#a",
                "removed\tL3:cargo:#a/(functions)",
                "moved\tL4:cargo:#a/(functions)/f -> L4:cargo:#z/(functions)/f",
            ],
        },
        Case {
            name: "type-with-a-method-added",
            base: &[MANIFEST, LIB, ("src/a.rs", ""), ("src/b.rs", "")],
            head: &[
                MANIFEST,
                LIB,
                (
                    "src/a.rs",
                    "pub struct S;\nimpl S {\n    pub fn new() -> Self { S }\n}\n",
                ),
                ("src/b.rs", ""),
            ],
            expect: &["added\tL3:cargo:#a/S", "added\tL4:cargo:#a/S/new"],
        },
        Case {
            name: "method-added-to-an-impl",
            base: &[
                MANIFEST,
                LIB,
                (
                    "src/a.rs",
                    "pub struct S;\nimpl S {\n    pub fn new() -> Self { S }\n}\n",
                ),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                (
                    "src/a.rs",
                    "pub struct S;\nimpl S {\n    pub fn new() -> Self { S }\n    pub fn len(&self) -> usize { 0 }\n}\n",
                ),
                ("src/b.rs", ""),
            ],
            expect: &["added\tL4:cargo:#a/S/len"],
        },
        Case {
            name: "field-added-to-a-public-struct-is-surface",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub struct P {\n    pub x: u8,\n}\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                (
                    "src/a.rs",
                    "pub struct P {\n    pub x: u8,\n    pub y: u8,\n}\n",
                ),
                ("src/b.rs", ""),
            ],
            expect: &["surface\tL3:cargo:#a/P"],
        },
        Case {
            name: "trait-impl-across-modules",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub trait T {}\n"),
                ("src/b.rs", "pub struct S;\n"),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub trait T {}\n"),
                ("src/b.rs", "pub struct S;\nimpl crate::a::T for S {}\n"),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:cargo:#b -> L2:cargo:#a\timplements",
                "+edge\tL3:cargo:#b/S -> L3:cargo:#a/T\timplements",
                "+edge\tL2:cargo:#b -> L2:cargo:#a\timplements",
            ],
        },
        Case {
            name: "comments-only",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                (
                    "src/a.rs",
                    "// A note.\npub fn f() {\n    // Why.\n    let _ = 1;\n}\n",
                ),
                ("src/b.rs", ""),
            ],
            expect: &[],
        },
        Case {
            name: "lines-shifted",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", "\n\n\npub fn f() { let _ = 1; }\n"),
                ("src/b.rs", ""),
            ],
            expect: &[],
        },
        Case {
            name: "use-of-an-undeclared-crate-is-not-a-dependency",
            base: &[
                MANIFEST,
                LIB,
                ("src/a.rs", ""),
                ("src/b.rs", "pub fn g() {}\n"),
            ],
            head: &[
                MANIFEST,
                LIB,
                ("src/a.rs", ""),
                ("src/b.rs", "use serde::Serialize;\npub fn g() {}\n"),
            ],
            expect: &[],
        },
        Case {
            name: "crate-dependency-declared",
            base: &[WS, CORE, CORE_LIB, APP, APP_MAIN],
            head: &[WS, CORE, CORE_LIB, APP_ON_CORE, APP_MAIN],
            expect: &[
                "finding\tnew-dependency\tL1:cargo:app# -> L1:cargo:core#\tdepends-on",
                "+edge\tL1:cargo:app# -> L1:cargo:core#\tdepends-on",
            ],
        },
        Case {
            name: "crate-cycle",
            base: &[WS, CORE, CORE_LIB, APP_ON_CORE, APP_MAIN],
            head: &[
                WS,
                (
                    "core/Cargo.toml",
                    "[package]\nname = \"core\"\n\n[dependencies]\napp = { path = \"../app\" }\n",
                ),
                CORE_LIB,
                APP_ON_CORE,
                APP_MAIN,
            ],
            expect: &[
                "finding\tnew-dependency\tL1:cargo:core# -> L1:cargo:app#\tdepends-on",
                "finding\tnew-cycle\tL1\tL1:cargo:app#, L1:cargo:core#\tnew: L1:cargo:core# -> L1:cargo:app#",
                "+edge\tL1:cargo:core# -> L1:cargo:app#\tdepends-on",
            ],
        },
        Case {
            name: "call-into-another-crate",
            base: &[WS, CORE, CORE_LIB, APP_ON_CORE, APP_MAIN],
            head: &[
                WS,
                CORE,
                CORE_LIB,
                APP_ON_CORE,
                ("app/src/main.rs", "fn main() { let _ = 6; core::c(); }\n"),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:cargo:app# -> L2:cargo:core#\tcalls",
                "changed\tL4:cargo:app#(functions)/main",
                "+edge\tL4:cargo:app#(functions)/main -> L4:cargo:core#(functions)/c\tcalls",
                "+edge\tL3:cargo:app#(functions) -> L3:cargo:core#(functions)\tcalls",
                "+edge\tL2:cargo:app# -> L2:cargo:core#\tcalls",
            ],
        },
        Case {
            name: "crate-added",
            base: &[
                ("Cargo.toml", "[workspace]\nmembers = [\"core\"]\n"),
                CORE,
                CORE_LIB,
            ],
            head: &[WS, CORE, CORE_LIB, APP_ON_CORE, APP_MAIN],
            expect: &[
                "finding\tnew-dependency\tL1:cargo:app# -> L1:cargo:core#\tdepends-on",
                "added\tL1:cargo:app#",
                "added\tL2:cargo:app#",
                "added\tL3:cargo:app#(functions)",
                "added\tL4:cargo:app#(functions)/main",
                // The manifest is a file no analyser reads: an `unknown`
                // component under the crate's own directory (§3.4).
                "added\tL2:cargo:app#.%2F",
                "added\tL3:cargo:app#.%2F/Cargo.toml",
                "+edge\tL1:cargo:app# -> L1:cargo:core#\tdepends-on",
            ],
        },
    ];
    assert!(cases.len() >= 30, "the exit test asks for at least 30");
    let mut failed = Vec::new();
    for case in cases {
        let dir = repo(case.name, case.base, case.head);
        let got = bp_diff(&dir);
        let mut want: Vec<String> = case.expect.iter().map(|s| (*s).to_owned()).collect();
        want.sort();
        if got != want {
            failed.push(format!("{}:\n  want {want:#?}\n  got  {got:#?}", case.name));
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} cases differ:\n{}",
        failed.len(),
        cases.len(),
        failed.join("\n")
    );
}

/// Change weight: lines removed plus lines added, split into signature and
/// body, rolled up to every ancestor.
#[test]
fn change_weight_counts_lines_by_node_and_rolls_them_up() {
    let weights = |name: &str, base: Files<'_>, head: Files<'_>| -> Vec<String> {
        let dir = repo(name, base, head);
        let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bp-diff-suite-cache");
        let out = Command::new(env!("CARGO_BIN_EXE_bp"))
            .args(["diff", dir.to_str().unwrap(), "HEAD~1", "HEAD"])
            .env("BLUEPRINT_CACHE_DIR", cache)
            .output()
            .unwrap();
        let mut w: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| l.starts_with("weight\t"))
            .map(str::to_owned)
            .collect();
        w.sort();
        w
    };
    // One line of signature and one of body changed: each is a line removed
    // and a line added.
    assert_eq!(
        weights(
            "weight-edit",
            &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {\n    let x = 1;\n}\n"),
                ("src/b.rs", "")
            ],
            &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f(y: u8) {\n    let x = 2;\n}\n"),
                ("src/b.rs", "")
            ],
        ),
        [
            "weight\tL1:cargo:#\t2\t2",
            "weight\tL2:cargo:#a\t2\t2",
            "weight\tL3:cargo:#a/(functions)\t2\t2",
            "weight\tL4:cargo:#a/(functions)/f\t2\t2",
        ]
    );
    // A new three-line function: its first line is signature.
    assert_eq!(
        weights(
            "weight-added",
            &[MANIFEST, LIB, ("src/a.rs", ""), ("src/b.rs", "")],
            &[
                MANIFEST,
                LIB,
                ("src/a.rs", ""),
                ("src/b.rs", "pub fn n() {\n    let _ = 1;\n}\n")
            ],
        ),
        [
            "weight\tL1:cargo:#\t1\t2",
            "weight\tL2:cargo:#b\t1\t2",
            "weight\tL3:cargo:#b/(functions)\t1\t2",
            "weight\tL4:cargo:#b/(functions)/n\t1\t2",
        ]
    );
    // A line outside any item counts for the file's module.
    assert_eq!(
        weights(
            "weight-module-line",
            &[
                MANIFEST,
                LIB,
                ("src/a.rs", "pub fn f() {}\n"),
                ("src/b.rs", "")
            ],
            &[
                MANIFEST,
                LIB,
                ("src/a.rs", "use std::fmt;\npub fn f() {}\n"),
                ("src/b.rs", "")
            ],
        ),
        ["weight\tL1:cargo:#\t0\t1", "weight\tL2:cargo:#a\t0\t1"]
    );
    // A file no analyser reads: all its changed lines, under its directory.
    assert_eq!(
        weights(
            "weight-unknown-file",
            &[
                MANIFEST,
                LIB,
                ("src/a.rs", ""),
                ("src/b.rs", ""),
                ("README.md", "one\ntwo\n")
            ],
            &[
                MANIFEST,
                LIB,
                ("src/a.rs", ""),
                ("src/b.rs", ""),
                ("README.md", "one\n2\nthree\n")
            ],
        ),
        [
            "weight\tL1:cargo:#\t0\t3",
            "weight\tL2:cargo:#.%2F\t0\t3",
            "weight\tL3:cargo:#.%2F/README.md\t0\t3",
        ]
    );
}
