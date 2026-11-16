//! L1 Cargo analysis over hand-written fixture workspaces and over this
//! repository, as text snapshots in `tests/snapshots/`. Set
//! `UPDATE_SNAPSHOTS=1` to rewrite them after an intended change.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{Diagnostic, cargo};
use blueprint_model::{Model, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let mut model = Model::default();
    let diags = cargo::analyse(&paths, &|p| files.get(p).cloned(), &mut model);
    (model, diags)
}

fn check(name: &str, entries: &[(&str, &str)]) -> (Model, Vec<Diagnostic>) {
    let (model, diags) = run(&files(entries));
    snapshot(name, &render(&model, &diags));
    (model, diags)
}

#[test]
fn single_crate() {
    let (model, diags) = check(
        "l1_cargo_single",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
            ),
            ("src/main.rs", "fn main() {}\n"),
        ],
    );
    assert_eq!(model.nodes().count(), 1);
    assert_eq!(
        model.edges().count(),
        0,
        "registry dependencies are not L1 edges"
    );
    assert!(diags.is_empty());
}

#[test]
fn virtual_workspace_with_inherited_build_and_target_dependencies() {
    check(
        "l1_cargo_virtual",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\", \"lib\", \"util\"]\n\n[workspace.dependencies]\nlib = { path = \"lib\" }\nserde = \"1\"\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\n\n[dependencies]\nlib = { workspace = true }\nserde.workspace = true\n\n[build-dependencies]\ngen = { path = \"../tools/gen\" }\n\n[target.'cfg(unix)'.dependencies]\nutil = { path = \"../util\" }\n\n[dev-dependencies]\nlib = { path = \"../lib\" }\n",
            ),
            ("lib/Cargo.toml", "[package]\nname = \"lib\"\n"),
            (
                "util/Cargo.toml",
                "[package]\nname = \"util\"\n[dependencies]\nlib = { path = \"../lib\" }\n",
            ),
            ("tools/gen/Cargo.toml", "[package]\nname = \"gen\"\n"),
        ],
    );
}

#[test]
fn globs_excludes_and_name_only_matches() {
    check(
        "l1_cargo_globs",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\", \"tools/**\"]\nexclude = [\"crates/old\"]\n\n[workspace.dependencies]\nb = { path = \"crates/b\" }\n",
            ),
            (
                "crates/a/Cargo.toml",
                "[package]\nname = \"a\"\n[dependencies]\nb = \"0.1\"\n",
            ),
            ("crates/b/Cargo.toml", "[package]\nname = \"b\"\n"),
            (
                "crates/old/Cargo.toml",
                "[package]\nname = \"old\"\n[dependencies]\nb = { workspace = true }\n",
            ),
            (
                "tools/x/y/Cargo.toml",
                "[package]\nname = \"y\"\n[dependencies]\nb.workspace = true\n",
            ),
            ("vendor/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ],
    );
}

#[test]
fn renamed_dependencies() {
    check(
        "l1_cargo_renamed",
        &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\n",
            ),
            (
                "a/Cargo.toml",
                "[package]\nname = \"a\"\n[dependencies]\nbee = { package = \"b\", path = \"../b\" }\n",
            ),
            ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            (
                "c/Cargo.toml",
                "[package]\nname = \"c\"\n[dependencies]\nalias = { package = \"b\", version = \"1\" }\nb = { path = \"../b\" }\n",
            ),
        ],
    );
}

#[test]
fn malformed_manifests_degrade_one_node_each() {
    let (model, diags) = check(
        "l1_cargo_malformed",
        &[
            ("Cargo.toml", "[workspace]\nmembers = [\"*\"]\n"),
            (
                "good/Cargo.toml",
                "[package]\nname = \"good\"\n[dependencies]\nbroken = { path = \"../broken\" }\nmissing = { path = \"../missing\" }\nescape = { path = \"../../outside\" }\n",
            ),
            ("broken/Cargo.toml", "[package\nname = \"broken\"\n"),
            ("nameless/Cargo.toml", "[package]\nversion = \"1.0.0\"\n"),
        ],
    );
    assert_eq!(model.nodes().count(), 3);
    assert_eq!(diags.len(), 4);
}

#[test]
fn this_repository_l1_model() {
    let (model, diags) = run(&common::this_repository(|name| name == "Cargo.toml"));
    snapshot("l1_cargo_blueprint", &render(&model, &diags));
    assert!(diags.is_empty(), "{diags:?}");
}

/// Mutated manifests never panic the analyser, and whatever it builds is a
/// valid model.
#[test]
fn mutated_manifests_never_panic() {
    let seeds = common::this_repository(|name| name == "Cargo.toml");
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tokens: [&[u8]; 10] = [
        b"[",
        b"]",
        b"\"",
        b"=",
        b"{",
        b"}",
        b"..",
        b"*",
        b"\n",
        b"workspace = true",
    ];
    for _ in 0..2_000 {
        let mut f = seeds.clone();
        for v in f.values_mut() {
            for _ in 0..(next() % 4) {
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
        }
        let (model, _) = run(&f);
        let json = serde_json::to_string(&model).unwrap();
        let back: Model = serde_json::from_str(&json).unwrap();
        assert_eq!(back.nodes().count(), model.nodes().count());
    }
}
