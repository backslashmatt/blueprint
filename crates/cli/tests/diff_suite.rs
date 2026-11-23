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

// An npm package `web` with TypeScript modules `src/a` and `src/b` (DR 0012).
const PKG: (&str, &str) = ("package.json", "{ \"name\": \"web\" }");
const TS_A: (&str, &str) = ("src/a.ts", "export function f() {}\n");
const TS_B_IMPORTS_A: (&str, &str) = (
    "src/b.ts",
    "import { f } from \"./a\";\nexport function g() {}\n",
);

// An npm workspace with packages `core` and `web`.
const NPM_WS: (&str, &str) = (
    "package.json",
    "{ \"private\": true, \"workspaces\": [\"p/*\"] }",
);
const NPM_CORE: (&str, &str) = ("p/core/package.json", "{ \"name\": \"core\" }");
const NPM_UTIL: (&str, &str) = ("p/core/src/util.ts", "export function helper() {}\n");
const NPM_WEB_ON_CORE: (&str, &str) = (
    "p/web/package.json",
    "{ \"name\": \"web\", \"dependencies\": { \"core\": \"workspace:*\" } }",
);
const NPM_WEB: (&str, &str) = ("p/web/package.json", "{ \"name\": \"web\" }");
const NPM_APP: (&str, &str) = ("p/web/src/app.ts", "export function run() { let x = 1; }\n");
const NPM_APP_CALLS: (&str, &str) = (
    "p/web/src/app.ts",
    "import { helper } from \"core/src/util\";\nexport function run() { let x = 1; helper(); }\n",
);

// A .NET project `App` with namespaces `App.A` and `App.B` (DR 0013).
const CSPROJ: (&str, &str) = (
    "App.csproj",
    "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>",
);
const CS_A: (&str, &str) = (
    "A.cs",
    "namespace App.A;\npublic class X { public static void F() { } }\n",
);
const CS_B: (&str, &str) = ("B.cs", "namespace App.B;\npublic class Y { }\n");
const CS_B_USES_A: (&str, &str) = (
    "B.cs",
    "using App.A;\nnamespace App.B;\npublic class Y { }\n",
);

// A Python project `app` (src layout) with modules `app.a` and `app.b`,
// and a uv workspace of projects `core` and `web` (DR 0014).
const PYPROJECT: (&str, &str) = ("pyproject.toml", "[project]\nname = \"app\"\n");
const PY_A: (&str, &str) = (
    "src/app/a.py",
    "def f(x):\n    \"\"\"Doc.\"\"\"\n    return x\n",
);
const PY_B: (&str, &str) = (
    "src/app/b.py",
    "class K:\n    def go(self):\n        return 1\n",
);
const PY_CORE: (&str, &str) = ("core/pyproject.toml", "[project]\nname = \"core\"\n");
const PY_WEB: (&str, &str) = ("web/pyproject.toml", "[project]\nname = \"web\"\n");

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
        Case {
            name: "ts-import-adds-a-module-dependency",
            base: &[PKG, TS_A, ("src/b.ts", "export function g() {}\n")],
            head: &[PKG, TS_A, TS_B_IMPORTS_A],
            expect: &[
                "finding\tnew-dependency\tL2:npm:#src/b -> L2:npm:#src/a\tdepends-on",
                "+edge\tL2:npm:#src/b -> L2:npm:#src/a\tdepends-on",
            ],
        },
        Case {
            name: "ts-call-across-modules",
            base: &[PKG, TS_A, TS_B_IMPORTS_A],
            head: &[
                PKG,
                TS_A,
                (
                    "src/b.ts",
                    "import { f } from \"./a\";\nexport function g() { f(); }\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:npm:#src/b -> L2:npm:#src/a\tcalls",
                "changed\tL4:npm:#src/b/(functions)/g",
                "+edge\tL4:npm:#src/b/(functions)/g -> L4:npm:#src/a/(functions)/f\tcalls",
                "+edge\tL3:npm:#src/b/(functions) -> L3:npm:#src/a/(functions)\tcalls",
                "+edge\tL2:npm:#src/b -> L2:npm:#src/a\tcalls",
            ],
        },
        Case {
            name: "ts-import-closes-a-module-cycle",
            base: &[PKG, TS_A, TS_B_IMPORTS_A],
            head: &[
                PKG,
                (
                    "src/a.ts",
                    "import { g } from \"./b\";\nexport function f() {}\n",
                ),
                TS_B_IMPORTS_A,
            ],
            expect: &[
                "finding\tnew-dependency\tL2:npm:#src/a -> L2:npm:#src/b\tdepends-on",
                "finding\tnew-cycle\tL2\tL2:npm:#src/a, L2:npm:#src/b\tnew: L2:npm:#src/a -> L2:npm:#src/b",
                "+edge\tL2:npm:#src/a -> L2:npm:#src/b\tdepends-on",
            ],
        },
        Case {
            name: "ts-body-only-change-is-contained",
            base: &[PKG, ("src/a.ts", "export function f() { return 1; }\n")],
            head: &[PKG, ("src/a.ts", "export function f() {\n  return 2;\n}\n")],
            expect: &["contained\tL4:npm:#src/a/(functions)/f"],
        },
        Case {
            name: "ts-exported-signature-change-is-surface",
            base: &[PKG, ("src/a.ts", "export function f(x: number) {}\n")],
            head: &[PKG, ("src/a.ts", "export function f(x: string) {}\n")],
            expect: &["surface\tL4:npm:#src/a/(functions)/f"],
        },
        Case {
            name: "ts-class-moved-with-its-method",
            base: &[
                PKG,
                ("src/a.ts", "export class C {\n  m() { return 1; }\n}\n"),
                ("src/b.ts", ""),
            ],
            head: &[
                PKG,
                ("src/a.ts", ""),
                ("src/b.ts", "export class C {\n  m() { return 1; }\n}\n"),
            ],
            expect: &[
                "moved\tL3:npm:#src/a/C -> L3:npm:#src/b/C",
                "moved\tL4:npm:#src/a/C/m -> L4:npm:#src/b/C/m",
            ],
        },
        Case {
            name: "ts-extends-across-modules",
            base: &[
                PKG,
                ("src/a.ts", "export class Base {}\n"),
                ("src/b.ts", "export class K {}\n"),
            ],
            head: &[
                PKG,
                ("src/a.ts", "export class Base {}\n"),
                (
                    "src/b.ts",
                    "import { Base } from \"./a\";\nexport class K extends Base {}\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:npm:#src/b -> L2:npm:#src/a\tdepends-on",
                "finding\tnew-dependency\tL2:npm:#src/b -> L2:npm:#src/a\textends",
                "surface\tL3:npm:#src/b/K",
                "+edge\tL3:npm:#src/b/K -> L3:npm:#src/a/Base\textends",
                "+edge\tL2:npm:#src/b -> L2:npm:#src/a\textends",
                "+edge\tL2:npm:#src/b -> L2:npm:#src/a\tdepends-on",
            ],
        },
        Case {
            name: "ts-call-into-a-declared-package",
            base: &[NPM_WS, NPM_CORE, NPM_UTIL, NPM_WEB_ON_CORE, NPM_APP],
            head: &[NPM_WS, NPM_CORE, NPM_UTIL, NPM_WEB_ON_CORE, NPM_APP_CALLS],
            expect: &[
                "finding\tnew-dependency\tL2:npm:p/web#src/app -> L2:npm:p/core#src/util\tdepends-on",
                "finding\tnew-dependency\tL2:npm:p/web#src/app -> L2:npm:p/core#src/util\tcalls",
                "changed\tL4:npm:p/web#src/app/(functions)/run",
                "+edge\tL4:npm:p/web#src/app/(functions)/run -> L4:npm:p/core#src/util/(functions)/helper\tcalls",
                "+edge\tL3:npm:p/web#src/app/(functions) -> L3:npm:p/core#src/util/(functions)\tcalls",
                "+edge\tL2:npm:p/web#src/app -> L2:npm:p/core#src/util\tcalls",
                "+edge\tL2:npm:p/web#src/app -> L2:npm:p/core#src/util\tdepends-on",
            ],
        },
        Case {
            name: "ts-import-of-an-undeclared-package-is-not-a-dependency",
            base: &[NPM_WS, NPM_CORE, NPM_UTIL, NPM_WEB, NPM_APP],
            head: &[NPM_WS, NPM_CORE, NPM_UTIL, NPM_WEB, NPM_APP_CALLS],
            expect: &["contained\tL4:npm:p/web#src/app/(functions)/run"],
        },
        Case {
            name: "cs-using-adds-a-namespace-dependency",
            base: &[CSPROJ, CS_A, CS_B],
            head: &[CSPROJ, CS_A, CS_B_USES_A],
            expect: &[
                "finding\tnew-dependency\tL2:dotnet:#App/B -> L2:dotnet:#App/A\tdepends-on",
                "+edge\tL2:dotnet:#App/B -> L2:dotnet:#App/A\tdepends-on",
            ],
        },
        Case {
            name: "cs-using-closes-a-namespace-cycle",
            base: &[CSPROJ, CS_A, CS_B_USES_A],
            head: &[
                CSPROJ,
                (
                    "A.cs",
                    "using App.B;\nnamespace App.A;\npublic class X { public static void F() { } }\n",
                ),
                CS_B_USES_A,
            ],
            expect: &[
                "finding\tnew-dependency\tL2:dotnet:#App/A -> L2:dotnet:#App/B\tdepends-on",
                "finding\tnew-cycle\tL2\tL2:dotnet:#App/A, L2:dotnet:#App/B\tnew: L2:dotnet:#App/A -> L2:dotnet:#App/B",
                "+edge\tL2:dotnet:#App/A -> L2:dotnet:#App/B\tdepends-on",
            ],
        },
        Case {
            name: "cs-interface-implemented",
            base: &[
                CSPROJ,
                ("A.cs", "namespace App.A;\npublic interface IRun { }\n"),
                CS_B_USES_A,
            ],
            head: &[
                CSPROJ,
                ("A.cs", "namespace App.A;\npublic interface IRun { }\n"),
                (
                    "B.cs",
                    "using App.A;\nnamespace App.B;\npublic class Y : IRun { }\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:dotnet:#App/B -> L2:dotnet:#App/A\timplements",
                "surface\tL3:dotnet:#App/B/Y",
                "+edge\tL3:dotnet:#App/B/Y -> L3:dotnet:#App/A/IRun\timplements",
                "+edge\tL2:dotnet:#App/B -> L2:dotnet:#App/A\timplements",
            ],
        },
        Case {
            name: "cs-static-call-across-namespaces",
            base: &[
                CSPROJ,
                CS_A,
                (
                    "B.cs",
                    "using App.A;\nnamespace App.B;\npublic class Y { void G() { int a = 1; } }\n",
                ),
            ],
            head: &[
                CSPROJ,
                CS_A,
                (
                    "B.cs",
                    "using App.A;\nnamespace App.B;\npublic class Y { void G() { int a = 1; X.F(); } }\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:dotnet:#App/B -> L2:dotnet:#App/A\tcalls",
                "changed\tL4:dotnet:#App/B/Y/G",
                // Its body changed and it gains an edge: changed (§3.3).
                "changed\tL3:dotnet:#App/B/Y",
                "+edge\tL4:dotnet:#App/B/Y/G -> L4:dotnet:#App/A/X/F\tcalls",
                "+edge\tL3:dotnet:#App/B/Y -> L3:dotnet:#App/A/X\tcalls",
                "+edge\tL2:dotnet:#App/B -> L2:dotnet:#App/A\tcalls",
            ],
        },
        Case {
            name: "py-body-and-docstring-change-is-contained",
            base: &[PYPROJECT, PY_A, PY_B],
            head: &[
                PYPROJECT,
                (
                    "src/app/a.py",
                    "def f(x):\n    \"\"\"Other.\"\"\"\n    return x + 1\n",
                ),
                PY_B,
            ],
            expect: &["contained\tL4:python:#app/a/(functions)/f"],
        },
        Case {
            name: "py-parameter-added-is-surface",
            base: &[PYPROJECT, PY_A, PY_B],
            head: &[
                PYPROJECT,
                (
                    "src/app/a.py",
                    "def f(x, y=0):\n    \"\"\"Doc.\"\"\"\n    return x\n",
                ),
                PY_B,
            ],
            expect: &["surface\tL4:python:#app/a/(functions)/f"],
        },
        Case {
            name: "py-method-body-in-a-class-is-contained",
            base: &[PYPROJECT, PY_A, PY_B],
            head: &[
                PYPROJECT,
                PY_A,
                (
                    "src/app/b.py",
                    "class K:\n    def go(self):\n        return 2\n",
                ),
            ],
            expect: &[
                "contained\tL3:python:#app/b/K",
                "contained\tL4:python:#app/b/K/go",
            ],
        },
        Case {
            name: "py-new-module-and-private-function",
            base: &[PYPROJECT, PY_A],
            head: &[
                PYPROJECT,
                PY_A,
                ("src/app/c.py", "def _helper():\n    pass\n"),
            ],
            expect: &[
                "added\tL2:python:#app/c",
                "added\tL3:python:#app/c/(functions)",
                "added\tL4:python:#app/c/(functions)/_helper",
            ],
        },
        Case {
            name: "py-workspace-dependency-added",
            base: &[PY_CORE, PY_WEB],
            head: &[
                PY_CORE,
                (
                    "web/pyproject.toml",
                    "[project]\nname = \"web\"\ndependencies = [\"core\"]\n[tool.uv.sources]\ncore = { workspace = true }\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL1:python:web# -> L1:python:core#\tdepends-on",
                "+edge\tL1:python:web# -> L1:python:core#\tdepends-on",
            ],
        },
        Case {
            name: "py-import-adds-a-module-dependency",
            base: &[PYPROJECT, PY_A, PY_B],
            head: &[
                PYPROJECT,
                PY_A,
                (
                    "src/app/b.py",
                    "from app.a import f\nclass K:\n    def go(self):\n        return 1\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:python:#app/b -> L2:python:#app/a\tdepends-on",
                "+edge\tL2:python:#app/b -> L2:python:#app/a\tdepends-on",
            ],
        },
        Case {
            name: "py-relative-import-closes-a-cycle",
            base: &[
                PYPROJECT,
                ("src/app/a.py", "from . import b\ndef f(x):\n    return x\n"),
                PY_B,
            ],
            head: &[
                PYPROJECT,
                ("src/app/a.py", "from . import b\ndef f(x):\n    return x\n"),
                (
                    "src/app/b.py",
                    "from .a import f\nclass K:\n    def go(self):\n        return 1\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:python:#app/b -> L2:python:#app/a\tdepends-on",
                "finding\tnew-cycle\tL2\tL2:python:#app/a, L2:python:#app/b\tnew: L2:python:#app/b -> L2:python:#app/a",
                "+edge\tL2:python:#app/b -> L2:python:#app/a\tdepends-on",
            ],
        },
        Case {
            name: "py-class-extends-an-imported-base",
            base: &[
                PYPROJECT,
                (
                    "src/app/a.py",
                    "class Base:\n    def run(self):\n        return 0\n",
                ),
                (
                    "src/app/b.py",
                    "import app.a\nclass K:\n    def go(self):\n        return self.run()\n",
                ),
            ],
            head: &[
                PYPROJECT,
                (
                    "src/app/a.py",
                    "class Base:\n    def run(self):\n        return 0\n",
                ),
                (
                    "src/app/b.py",
                    "import app.a\nclass K(app.a.Base):\n    def go(self):\n        return self.run()\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:python:#app/b -> L2:python:#app/a\tcalls",
                "finding\tnew-dependency\tL2:python:#app/b -> L2:python:#app/a\textends",
                "surface\tL3:python:#app/b/K",
                // `self.run()` now resolves through the base.
                "+edge\tL4:python:#app/b/K/go -> L4:python:#app/a/Base/run\tcalls",
                "+edge\tL3:python:#app/b/K -> L3:python:#app/a/Base\tcalls",
                "+edge\tL3:python:#app/b/K -> L3:python:#app/a/Base\textends",
                "+edge\tL2:python:#app/b -> L2:python:#app/a\tcalls",
                "+edge\tL2:python:#app/b -> L2:python:#app/a\textends",
            ],
        },
        Case {
            name: "py-call-through-an-import",
            base: &[
                PYPROJECT,
                PY_A,
                (
                    "src/app/b.py",
                    "from app.a import f\ndef g():\n    return 1\n",
                ),
            ],
            head: &[
                PYPROJECT,
                PY_A,
                (
                    "src/app/b.py",
                    "from app.a import f\ndef g():\n    return f(1)\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:python:#app/b -> L2:python:#app/a\tcalls",
                "changed\tL4:python:#app/b/(functions)/g",
                "+edge\tL4:python:#app/b/(functions)/g -> L4:python:#app/a/(functions)/f\tcalls",
                "+edge\tL3:python:#app/b/(functions) -> L3:python:#app/a/(functions)\tcalls",
                "+edge\tL2:python:#app/b -> L2:python:#app/a\tcalls",
            ],
        },
        Case {
            name: "py-import-from-a-dependency-project",
            base: &[
                PY_CORE,
                ("core/core/util.py", "def helper():\n    return 1\n"),
                (
                    "web/pyproject.toml",
                    "[project]\nname = \"web\"\ndependencies = [\"core\"]\n",
                ),
                ("web/web/app.py", "def run():\n    return 0\n"),
            ],
            head: &[
                PY_CORE,
                ("core/core/util.py", "def helper():\n    return 1\n"),
                (
                    "web/pyproject.toml",
                    "[project]\nname = \"web\"\ndependencies = [\"core\"]\n",
                ),
                (
                    "web/web/app.py",
                    "from core.util import helper\ndef run():\n    return helper()\n",
                ),
            ],
            expect: &[
                "finding\tnew-dependency\tL2:python:web#web/app -> L2:python:core#core/util\tcalls",
                "finding\tnew-dependency\tL2:python:web#web/app -> L2:python:core#core/util\tdepends-on",
                "changed\tL4:python:web#web/app/(functions)/run",
                "+edge\tL4:python:web#web/app/(functions)/run -> L4:python:core#core/util/(functions)/helper\tcalls",
                "+edge\tL3:python:web#web/app/(functions) -> L3:python:core#core/util/(functions)\tcalls",
                "+edge\tL2:python:web#web/app -> L2:python:core#core/util\tcalls",
                "+edge\tL2:python:web#web/app -> L2:python:core#core/util\tdepends-on",
            ],
        },
        Case {
            name: "py-import-of-an-undeclared-project-is-not-a-dependency",
            base: &[
                PY_CORE,
                ("core/core/util.py", "def helper():\n    return 1\n"),
                PY_WEB,
                ("web/web/app.py", "def run():\n    return 0\n"),
            ],
            head: &[
                PY_CORE,
                ("core/core/util.py", "def helper():\n    return 1\n"),
                PY_WEB,
                (
                    "web/web/app.py",
                    "from core.util import helper\ndef run():\n    return helper()\n",
                ),
            ],
            expect: &["contained\tL4:python:web#web/app/(functions)/run"],
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
