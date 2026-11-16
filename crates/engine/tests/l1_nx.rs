//! L1 Nx analysis over fixtures, as text snapshots in `tests/snapshots/`.
//! Set `UPDATE_SNAPSHOTS=1` to rewrite them.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{Diagnostic, nx};
use blueprint_model::{Confidence, Model, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let mut model = Model::default();
    let diags = nx::analyse(&paths, &|p| files.get(p).cloned(), &mut model);
    (model, diags)
}

fn edges(model: &Model) -> Vec<String> {
    model
        .edges()
        .map(|e| {
            format!(
                "{} -> {} {:?}",
                e.key.from.system_key().dir,
                e.key.to.system_key().dir,
                e.confidence
            )
        })
        .collect()
}

#[test]
fn implicit_dependencies() {
    let (model, diags) = run(&files(&[
        ("nx.json", "{}"),
        (
            "apps/web/project.json",
            r#"{ "name": "web", "implicitDependencies": ["@acme/ui", "feature-*", "!feature-old", "ghost"],
                 "targets": { "build": { "dependsOn": ["^build"] } } }"#,
        ),
        ("libs/ui/project.json", r#"{ "name": "@acme/ui" }"#),
        ("libs/feature-a/project.json", r#"{ "name": "feature-a" }"#),
        (
            "libs/feature-old/project.json",
            r#"{ "name": "feature-old" }"#,
        ),
        ("libs/unnamed/project.json", "{}"),
        ("libs/broken/project.json", "{ \"name\": "),
        ("node_modules/x/project.json", r#"{ "name": "x" }"#),
    ]));
    snapshot("l1_nx", &render(&model, &diags));
    assert_eq!(
        edges(&model),
        vec![
            "apps/web -> libs/feature-a Inferred",
            "apps/web -> libs/ui Exact"
        ],
        "names are exact, globs inferred, ! removes"
    );
    assert!(
        model.nodes().any(|n| n.name == "unnamed"),
        "the directory names a nameless project"
    );
    let unknown: Vec<String> = model
        .nodes()
        .filter(|n| n.confidence == Confidence::Unknown)
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(unknown, vec!["broken"]);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("ghost names no project")),
        "{diags:?}"
    );
}

#[test]
fn mutated_projects_never_panic() {
    let seed = files(&[
        (
            "a/project.json",
            r#"{ "name": "a", "implicitDependencies": ["b", "c*", "!c2"] }"#,
        ),
        ("b/project.json", r#"{ "name": "b" }"#),
        ("c1/project.json", r#"{ "name": "c1" }"#),
    ]);
    let mut state = 0xA5A5_5A5A_1234_4321u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tokens: [&[u8]; 8] = [b"{", b"}", b"\"", b"[", b"]", b"*", b"!", b"../.."];
    for _ in 0..3_000 {
        let mut f = seed.clone();
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
        let back: Model = serde_json::from_str(&serde_json::to_string(&model).unwrap()).unwrap();
        assert_eq!(back.nodes().count(), model.nodes().count());
    }
}
