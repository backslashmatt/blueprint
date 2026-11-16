//! L2 analysis over fixtures and over this repository, as text snapshots in
//! `tests/snapshots/`. Set `UPDATE_SNAPSHOTS=1` to rewrite them after an
//! intended change.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{Diagnostic, cargo};
use blueprint_engine::l2;
use blueprint_model::{Confidence, Level, Model, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let read = |p: &RepoPath| files.get(p).cloned();
    let mut model = Model::default();
    let mut diags = cargo::analyse(&paths, &read, &mut model);
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
        confidence(&model, "cli"),
        Some(Confidence::Exact),
        "binary-only modules are included"
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
