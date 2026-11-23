//! L2 analysis over fixtures and over this repository, as text snapshots in
//! `tests/snapshots/`. Set `UPDATE_SNAPSHOTS=1` to rewrite them after an
//! intended change.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{self, Diagnostic};
use blueprint_engine::l2;
use blueprint_model::{Confidence, Level, Model, NodeKind, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let read = |p: &RepoPath| files.get(p).cloned();
    let mut model = Model::default();
    let mut diags = l1::analyse(&paths, &read, &mut model);
    diags.extend(l2::analyse(&paths, &read, &mut model));
    (model, diags)
}

fn check(name: &str, entries: &[(&str, &str)]) -> (Model, Vec<Diagnostic>) {
    let (model, diags) = run(&files(entries));
    snapshot(name, &render(&model, &diags));
    (model, diags)
}

fn confidence(model: &Model, name: &str) -> Option<Confidence> {
    model
        .level(Level::Module)
        .find(|n| n.name == name)
        .map(|n| n.confidence)
}

#[test]
fn rust_module_tree_follows_the_reference_rules() {
    let (model, _) = check(
        "l2_rust_modules",
        &[
            ("Cargo.toml", "[package]\nname = \"k\"\n"),
            (
                "src/lib.rs",
                "//! crate docs\npub mod a;\nmod b;\n#[cfg(test)]\nmod tests;\n#[path = \"gen/out.rs\"]\npub(crate) mod generated;\npub mod inline {\n    pub mod deep;\n    #[path = \"elsewhere.rs\"]\n    mod moved;\n}\nmod gone;\nmod r#type;\n",
            ),
            ("src/a.rs", "mod nested;\n"),
            ("src/a/nested.rs", "fn f() {}\n"),
            ("src/b/mod.rs", "pub mod c;\n"),
            ("src/b/c.rs", ""),
            ("src/tests.rs", ""),
            ("src/gen/out.rs", "mod sibling;\n"),
            ("src/gen/sibling.rs", ""),
            ("src/inline/deep.rs", ""),
            ("src/inline/elsewhere.rs", ""),
            ("src/type.rs", ""),
            ("src/main.rs", "mod cli;\nmod a;\n"),
            ("src/cli.rs", ""),
        ],
    );
    assert_eq!(confidence(&model, "a::nested"), Some(Confidence::Exact));
    assert_eq!(confidence(&model, "generated"), Some(Confidence::Exact));
    assert_eq!(
        confidence(&model, "generated::sibling"),
        Some(Confidence::Exact)
    );
    assert_eq!(confidence(&model, "inline::moved"), Some(Confidence::Exact));
    assert_eq!(confidence(&model, "gone"), Some(Confidence::Inferred));
    assert_eq!(confidence(&model, "type"), Some(Confidence::Exact));
    assert_eq!(
        confidence(&model, "(bin main)::cli"),
        Some(Confidence::Exact),
        "binary-only modules are included, in the binary's own namespace"
    );
}

#[test]
fn a_malformed_source_degrades_one_node() {
    let (model, diags) = check(
        "l2_rust_malformed",
        &[
            ("Cargo.toml", "[package]\nname = \"k\"\n"),
            ("src/lib.rs", "pub mod good;\npub mod bad;\nmod binary;\n"),
            ("src/good.rs", "pub fn ok() {}\n"),
            ("src/bad.rs", "pub fn broken( {\nmod inner;\n"),
            ("src/bad/inner.rs", ""),
            ("src/binary.rs", "\u{0}\u{1}not rust at all ]]]"),
        ],
    );
    assert_eq!(confidence(&model, "good"), Some(Confidence::Exact));
    assert_eq!(confidence(&model, "bad"), Some(Confidence::Unknown));
    assert_eq!(confidence(&model, "binary"), Some(Confidence::Unknown));
    assert!(!diags.is_empty());
}

#[test]
fn systems_without_an_analyser_and_unowned_files_fall_back_to_directories() {
    let (model, _) = check(
        "l2_fallback",
        &[
            ("Cargo.toml", "[workspace]\nmembers = [\"tool\", \"lib\"]\n"),
            ("tool/Cargo.toml", "[package]\nname = \"tool\"\n"),
            ("tool/build.rs", "fn main() {}\n"),
            ("tool/scripts/run.sh", "echo\n"),
            ("tool/assets/logo.svg", "<svg/>"),
            ("lib/Cargo.toml", "[package]\nname = \"lib\"\n"),
            ("lib/src/lib.rs", ""),
            ("docs/guide.md", "# guide\n"),
            ("docs/img/a.png", ""),
            ("editors/code/package.json", "{}"),
            ("README.md", "# readme\n"),
        ],
    );
    let unowned = model
        .level(Level::System)
        .find(|n| n.name == l2::UNOWNED_NAME);
    assert!(unowned.is_some(), "files outside every system are kept");
}

#[test]
fn this_repository_l1_l2_model() {
    let repo = common::this_repository(|_| true);
    let (model, diags) = run(&repo);
    snapshot("l2_blueprint", &render(&model, &diags));
    assert!(model.level(Level::Module).count() > 10);
}

/// Mutated Rust sources never panic the analyser, and it always builds a
/// valid model.
#[test]
fn mutated_sources_never_panic() {
    let seeds = common::this_repository(|name| name.ends_with(".rs") || name == "Cargo.toml");
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tokens: [&[u8]; 10] = [
        b"mod x;",
        b"{",
        b"}",
        b"#[path = \"../../a.rs\"]",
        b"\"",
        b"mod ",
        b";",
        b"r#",
        b"/*",
        b"\n",
    ];
    let rust_files: Vec<RepoPath> = seeds
        .keys()
        .filter(|p| p.to_string().ends_with(".rs"))
        .cloned()
        .collect();
    for _ in 0..300 {
        let mut f = seeds.clone();
        for _ in 0..8 {
            let p = &rust_files[(next() as usize) % rust_files.len()];
            let v = f.get_mut(p).unwrap();
            let i = (next() as usize) % (v.len() + 1);
            match next() % 3 {
                0 => v.truncate(i),
                1 => {
                    let t = tokens[(next() % tokens.len() as u64) as usize];
                    v.splice(i..i, t.iter().copied());
                }
                _ => {
                    if i < v.len() {
                        v[i] = next() as u8;
                    }
                }
            }
        }
        let (model, _) = run(&f);
        let json = serde_json::to_string(&model).unwrap();
        let back: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(back.nodes().count(), model.nodes().count());
    }
}

fn names(model: &Model) -> Vec<String> {
    model.level(Level::Module).map(|n| n.name.clone()).collect()
}

#[test]
fn deeply_nested_inline_modules_stop_at_the_depth_limit() {
    let src = format!("{}{}", "mod a{".repeat(900), "}".repeat(900));
    let start = std::time::Instant::now();
    let (model, diags) = run(&files(&[
        ("Cargo.toml", "[package]\nname = \"k\"\n"),
        ("src/lib.rs", &src),
    ]));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
    // The nested modules up to the limit (the crate root is the system).
    let modules = model
        .level(Level::Module)
        .filter(|n| n.kind == NodeKind::Module)
        .count();
    assert_eq!(modules, l2::rust::MAX_DEPTH + 1);
    assert!(
        diags.iter().any(|d| d.message.contains("deeper than")),
        "{diags:?}"
    );
}

#[test]
fn reference_rules_for_path_on_inline_modules_and_repeated_files() {
    let (model, diags) = check(
        "l2_rust_paths",
        &[
            ("Cargo.toml", "[package]\nname = \"k\"\n"),
            (
                "src/lib.rs",
                concat!(
                    "#[path = \"thread_files\"]\nmod thread {\n    #[path = \"tls.rs\"]\n    mod local_data;\n    mod other;\n}\n",
                    "#[path = \"shared/m.rs\"]\nmod one;\n#[path = \"shared/m.rs\"]\nmod two;\n",
                    "#[path = \"loop.rs\"]\nmod looping;\n",
                ),
            ),
            ("src/thread_files/tls.rs", ""),
            ("src/thread_files/other.rs", ""),
            ("src/shared/m.rs", "mod leaf;\n"),
            ("src/shared/leaf.rs", ""),
            ("src/loop.rs", "#[path = \"loop.rs\"]\nmod again;\n"),
        ],
    );
    assert_eq!(
        confidence(&model, "thread::local_data"),
        Some(Confidence::Exact)
    );
    assert_eq!(confidence(&model, "thread::other"), Some(Confidence::Exact));
    assert_eq!(
        confidence(&model, "two"),
        Some(Confidence::Exact),
        "a file may be included twice"
    );
    assert_eq!(confidence(&model, "two::leaf"), Some(Confidence::Exact));
    assert_eq!(
        confidence(&model, "looping::again"),
        Some(Confidence::Inferred),
        "{diags:?}"
    );
}

#[test]
fn cfg_variants_and_macro_declared_modules_are_merged() {
    let (model, _) = check(
        "l2_rust_cfg",
        &[
            ("Cargo.toml", "[package]\nname = \"k\"\n"),
            (
                "src/lib.rs",
                concat!(
                    "#[cfg(unix)]\n#[path = \"sys/unix.rs\"]\nmod sys;\n#[cfg(windows)]\n#[path = \"sys/windows.rs\"]\nmod sys;\n",
                    "#[cfg(unix)]\nmod imp { pub mod a {} }\n#[cfg(not(unix))]\nmod imp { pub mod b {} }\n",
                    "cfg_if::cfg_if! {\n    if #[cfg(unix)] { mod unix_only; } else { mod other_os; }\n}\n",
                    "#[cfg_attr(feature = \"pathological\", allow(dead_code))]\nmod plain;\n",
                    "#[cfg_attr(test, path = \"alt.rs\")]\nmod switched;\n",
                ),
            ),
            ("src/sys/unix.rs", "pub mod fd;\n"),
            ("src/sys/fd.rs", ""),
            ("src/sys/windows.rs", "pub mod handle;\n"),
            ("src/sys/handle.rs", ""),
            ("src/unix_only.rs", ""),
            ("src/other_os.rs", ""),
            ("src/plain.rs", ""),
            ("src/switched.rs", ""),
        ],
    );
    let n = names(&model);
    for m in ["sys::fd", "sys::handle", "imp::a", "imp::b"] {
        assert!(n.contains(&m.to_owned()), "{m} missing from {n:?}");
    }
    assert_eq!(confidence(&model, "unix_only"), Some(Confidence::Inferred));
    assert_eq!(confidence(&model, "other_os"), Some(Confidence::Inferred));
    assert_eq!(
        confidence(&model, "plain"),
        Some(Confidence::Exact),
        "cfg_attr without path"
    );
    assert_eq!(
        confidence(&model, "switched"),
        Some(Confidence::Inferred),
        "cfg_attr with path"
    );
}

#[test]
fn crate_roots_come_from_the_manifest() {
    let (model, _) = check(
        "l2_rust_targets",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"custom\", \"bins\"]\n",
            ),
            (
                "custom/Cargo.toml",
                "[package]\nname = \"custom\"\n[lib]\npath = \"lib.rs\"\n[[bin]]\nname = \"t\"\npath = \"tools/t.rs\"\n",
            ),
            ("custom/lib.rs", "pub mod a;\n"),
            ("custom/a.rs", ""),
            ("custom/tools/t.rs", "mod helper;\n"),
            ("custom/tools/helper.rs", ""),
            ("bins/Cargo.toml", "[package]\nname = \"bins\"\n"),
            ("bins/src/lib.rs", "pub mod core;\n"),
            ("bins/src/core.rs", ""),
            ("bins/src/bin/tool.rs", "mod cli;\n"),
            ("bins/src/bin/cli.rs", ""),
            ("bins/src/bin/big/main.rs", "mod parts;\n"),
            ("bins/src/bin/big/parts.rs", ""),
        ],
    );
    let n = names(&model);
    // The library is the primary crate; every binary has a namespace.
    for m in [
        "a",
        "(bin t)::helper",
        "core",
        "(bin tool)::cli",
        "(bin big)::parts",
    ] {
        assert!(n.contains(&m.to_owned()), "{m} missing from {n:?}");
    }
}

#[test]
fn files_outside_the_module_tree_still_appear() {
    let (model, _) = check(
        "l2_rust_rest",
        &[
            ("Cargo.toml", "[package]\nname = \"root\"\n[workspace]\n"),
            ("src/lib.rs", "pub mod a;\nmod i {}\nfn broken( {\n"),
            ("src/a.rs", ""),
            ("src/orphan.rs", ""),
            ("tests/it.rs", ""),
            ("benches/b.rs", ""),
            ("docs/guide.md", ""),
            ("build.rs", ""),
        ],
    );
    let n = names(&model);
    for d in ["src/", "tests/", "benches/", "docs/"] {
        assert!(n.contains(&d.to_owned()), "{d} missing from {n:?}");
    }
    assert_eq!(
        confidence(&model, "i"),
        Some(Confidence::Unknown),
        "inline in a broken file"
    );
}

#[test]
fn module_visibility_is_recorded() {
    let (model, _) = run(&files(&[
        ("Cargo.toml", "[package]\nname = \"k\"\n"),
        (
            "src/lib.rs",
            "pub mod a;\npub(crate) mod b;\nmod c;\npub(in crate::a) mod d {}\ncfg_if::cfg_if! { if #[cfg(unix)] { pub mod e; } }\n",
        ),
        ("src/a.rs", ""),
        ("src/b.rs", ""),
        ("src/c.rs", ""),
        ("src/e.rs", ""),
    ]));
    let vis = |name: &str| {
        model
            .level(Level::Module)
            .find(|n| n.name == name)
            .and_then(|n| n.visibility)
    };
    use blueprint_model::Visibility::{Private, Public, Restricted};
    assert_eq!(
        ["a", "b", "c", "d", "e"].map(vis),
        [
            Some(Public),
            Some(Restricted),
            Some(Private),
            Some(Restricted),
            Some(Public)
        ]
    );
}

fn modules(model: &Model) -> Vec<String> {
    model
        .level(Level::Module)
        .filter(|n| n.kind == NodeKind::Module)
        .map(|n| format!("{}", n.id))
        .collect()
}

#[test]
fn systems_sharing_a_directory_each_get_their_own_languages_files() {
    // A maturin crate: Cargo and Python in one directory.
    let (model, _) = run(&files(&[
        ("Cargo.toml", "[package]\nname = \"ext\"\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
        ("pyproject.toml", "[project]\nname = \"ext\"\n"),
        ("python/ext/__init__.py", ""),
        ("python/ext/util.py", "def g(): pass\n"),
    ]));
    let got = modules(&model);
    assert!(got.contains(&"L2:python:#python/ext".to_owned()), "{got:?}");
    assert!(
        got.contains(&"L2:python:#python/ext/util".to_owned()),
        "{got:?}"
    );
    // npm and Python in one directory; a `.py` file is never an npm module.
    let (model, _) = run(&files(&[
        ("package.json", "{ \"name\": \"web\" }"),
        ("pyproject.toml", "[project]\nname = \"tools\"\n"),
        ("src/a.ts", "export function f() {}\n"),
        ("src/a.py", "print(1)\n"),
        ("app/__init__.py", ""),
    ]));
    let got = modules(&model);
    assert!(got.contains(&"L2:npm:#src/a".to_owned()), "{got:?}");
    assert!(got.contains(&"L2:python:#app".to_owned()), "{got:?}");
    // `src/` is the import root: `src/a.py` is `a`.
    assert!(got.contains(&"L2:python:#a".to_owned()), "{got:?}");
    let a = model
        .level(Level::Module)
        .find(|n| n.id.to_string() == "L2:npm:#src/a")
        .unwrap();
    assert_eq!(a.source.to_string(), "src/a.ts");
    // A `.py` file in an npm package alone is shown by directory.
    let (model, _) = run(&files(&[
        ("package.json", "{ \"name\": \"web\" }"),
        ("scripts/build.py", "print(1)\n"),
    ]));
    assert!(modules(&model).is_empty(), "{:?}", modules(&model));
}

#[test]
fn dunder_modules_are_public() {
    let (model, _) = run(&files(&[
        ("pyproject.toml", "[project]\nname = \"p\"\n"),
        ("pkg/__main__.py", ""),
        ("pkg/_impl.py", ""),
    ]));
    let vis = |id: &str| {
        model
            .level(Level::Module)
            .find(|n| n.id.to_string() == id)
            .and_then(|n| n.visibility)
    };
    assert_eq!(
        vis("L2:python:#pkg/__main__"),
        Some(blueprint_model::Visibility::Public)
    );
    assert_eq!(
        vis("L2:python:#pkg/_impl"),
        Some(blueprint_model::Visibility::Private)
    );
}

#[test]
fn a_crate_keeps_the_js_file_its_path_module_reaches_beside_package_json() {
    let (model, diags) = run(&files(&[
        ("Cargo.toml", "[package]\nname = \"c\"\n"),
        ("package.json", "{ \"name\": \"c\" }"),
        (
            "src/lib.rs",
            "#[path = \"gen.js\"]\nmod gen;\npub fn top() {}\n",
        ),
        ("src/gen.js", "export function a() {}\n"),
        ("src/other.ts", "export function b() {}\n"),
    ]));
    assert!(diags.is_empty(), "{diags:?}");
    let module = model
        .level(Level::Module)
        .find(|n| n.id.to_string() == "L2:cargo:#gen")
        .unwrap();
    assert_eq!(module.source.to_string(), "src/gen.js");
    let got = modules(&model);
    assert!(!got.contains(&"L2:npm:#src/gen".to_owned()), "{got:?}");
    assert!(got.contains(&"L2:npm:#src/other".to_owned()), "{got:?}");
}
