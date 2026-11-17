//! L1 npm, yarn and pnpm analysis over fixtures, as text snapshots in
//! `tests/snapshots/`. Set `UPDATE_SNAPSHOTS=1` to rewrite them.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{Diagnostic, npm};
use blueprint_model::{Confidence, Model, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let mut model = Model::default();
    let diags = npm::analyse(&paths, &|p| files.get(p).cloned(), &mut model);
    (model, diags)
}

fn check(name: &str, entries: &[(&str, &str)]) -> (Model, Vec<Diagnostic>) {
    let (model, diags) = run(&files(entries));
    snapshot(name, &render(&model, &diags));
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
fn npm_workspaces_with_protocols_and_name_matches() {
    let (model, diags) = check(
        "l1_npm_workspaces",
        &[
            (
                "package.json",
                r#"{ "name": "root", "private": true, "workspaces": ["packages/*", "apps/web"] }"#,
            ),
            (
                "apps/web/package.json",
                r#"{ "name": "web", "dependencies": { "ui": "workspace:*", "utils": "^1.0.0", "react": "^18" },
                    "devDependencies": { "testkit": "workspace:*" } }"#,
            ),
            (
                "packages/ui/package.json",
                r#"{ "name": "ui", "peerDependencies": { "utils": "*" } }"#,
            ),
            (
                "packages/utils/package.json",
                r#"{ "name": "utils", "optionalDependencies": { "local": "file:../../vendor/local" } }"#,
            ),
            ("packages/testkit/package.json", r#"{ "name": "testkit" }"#),
            ("vendor/local/package.json", r#"{ "name": "local" }"#),
            ("node_modules/react/package.json", r#"{ "name": "react" }"#),
        ],
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        edges(&model),
        vec![
            "apps/web -> packages/ui Exact",
            "apps/web -> packages/utils Inferred",
            "packages/ui -> packages/utils Inferred",
            "packages/utils -> vendor/local Exact"
        ]
    );
    assert!(
        model
            .nodes()
            .all(|n| !n.id.system_key().dir.to_string().contains("node_modules"))
    );
}

#[test]
fn yarn_object_form_and_pnpm_yaml_with_negation() {
    let (model, _) = check(
        "l1_npm_yarn_pnpm",
        &[
            (
                "yarn/package.json",
                r#"{ "workspaces": { "packages": ["libs/*"], "nohoist": ["**/x"] } }"#,
            ),
            (
                "yarn/libs/a/package.json",
                r#"{ "name": "a", "dependencies": { "b": "1.x" } }"#,
            ),
            ("yarn/libs/b/package.json", r#"{ "name": "b" }"#),
            ("pnpm/package.json", r#"{ "name": "pnpm-root" }"#),
            (
                "pnpm/pnpm-workspace.yaml",
                "packages:\n  - 'pkgs/*'\n  - \"!pkgs/skip\"   # not this one\ncatalog:\n  react: ^18\n",
            ),
            (
                "pnpm/pkgs/x/package.json",
                r#"{ "name": "x", "dependencies": { "y": "workspace:^", "skip": "*", "a": "*" } }"#,
            ),
            ("pnpm/pkgs/y/package.json", r#"{ "name": "y" }"#),
            ("pnpm/pkgs/skip/package.json", r#"{ "name": "skip" }"#),
        ],
    );
    assert_eq!(
        edges(&model),
        vec![
            "pnpm/pkgs/x -> pnpm/pkgs/y Exact",
            "yarn/libs/a -> yarn/libs/b Inferred"
        ]
    );
    assert!(
        model.nodes().any(|n| n.name == "pnpm-root"),
        "a named root is a system too"
    );
}

#[test]
fn external_specs_and_malformed_manifests() {
    let (model, diags) = check(
        "l1_npm_malformed",
        &[
            ("package.json", r#"{ "workspaces": ["*"] }"#),
            (
                "app/package.json",
                r#"{ "name": "app", "dependencies": {
                    "lib": "github:someone/lib", "lib2": "git+https://example.invalid/lib.git",
                    "alias": "npm:lib@1", "short": "someone/lib", "missing": "workspace:*", "broken": "*" } }"#,
            ),
            ("lib/package.json", r#"{ "name": "lib" }"#),
            ("lib2/package.json", r#"{ "name": "lib2" }"#),
            ("broken/package.json", r#"{ "name": "broken", "#),
            ("array/package.json", "[1, 2, 3]"),
        ],
    );
    assert_eq!(
        edges(&model),
        Vec::<String>::new(),
        "git, URL, alias and shorthand specs are external"
    );
    let unknown: Vec<String> = model
        .nodes()
        .filter(|n| n.confidence == Confidence::Unknown)
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(unknown, vec!["array", "broken"]);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("`missing` uses workspace:")),
        "{diags:?}"
    );
}

#[test]
fn mutated_manifests_never_panic() {
    let seed = files(&[
        (
            "package.json",
            r#"{ "workspaces": ["packages/*", "!packages/x"] }"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - 'a/*'\n"),
        (
            "packages/a/package.json",
            r#"{ "name": "a", "dependencies": { "b": "workspace:*", "c": "file:../c" } }"#,
        ),
        (
            "packages/b/package.json",
            r#"{ "name": "b", "peerDependencies": { "a": "^1" } }"#,
        ),
    ]);
    let mut state = 0xDEAD_BEEF_CAFE_F00Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tokens: [&[u8]; 9] = [
        b"{",
        b"}",
        b"\"",
        b":",
        b",",
        b"workspace:",
        b"file:../../..",
        b"[",
        b"\\u0000",
    ];
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

#[test]
fn review_findings_roots_pnpm_forms_paths_and_globs() {
    let (model, diags) = check(
        "l1_npm_edge_cases",
        &[
            // A root that is also a package, with a BOM; members depend on it.
            (
                "package.json",
                "\u{feff}{ \"name\": \"root\", \"workspaces\": [\"p/{a,b}\", \"q/[c]\", \"p/a/docs\"], \"dependencies\": { \"a\": \"workspace:*\" } }",
            ),
            (
                "p/a/package.json",
                r#"{ "name": "a", "workspaces": ["plugins/*"], "dependencies": { "root": "workspace:*", "b": "./tarballs/b-1.0.0.tgz", "c": "../../q/c" } }"#,
            ),
            (
                "p/a/plugins/x/package.json",
                r#"{ "name": "x", "dependencies": { "a": "workspace:*" } }"#,
            ),
            (
                "p/a/docs/package.json",
                r#"{ "name": "docs", "dependencies": { "b": "workspace:*" } }"#,
            ),
            ("p/b/package.json", r#"{ "name": "b" }"#),
            ("q/c/package.json", r#"{ "name": "c" }"#),
            // pnpm: flow-style list; plain ranges are not linked.
            (
                "pn/pnpm-workspace.yaml",
                "packages: ['libs/*', \"!libs/old\"]\n",
            ),
            (
                "pn/libs/m/package.json",
                r#"{ "name": "m", "dependencies": { "n": "^1", "o": "workspace:*" } }"#,
            ),
            ("pn/libs/n/package.json", r#"{ "name": "n" }"#),
            ("pn/libs/o/package.json", r#"{ "name": "o" }"#),
            // A settings-only pnpm yaml is not a root.
            (
                "solo/pnpm-workspace.yaml",
                "onlyBuiltDependencies:\n  - esbuild\n",
            ),
            (
                "solo/package.json",
                r#"{ "name": "solo", "dependencies": { "c": "file:../q/c" } }"#,
            ),
        ],
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        edges(&model),
        vec![
            " -> p/a Exact",
            "p/a ->  Exact",
            "p/a -> q/c Exact",
            "p/a/docs -> p/b Exact",
            "p/a/plugins/x -> p/a Exact",
            "pn/libs/m -> pn/libs/o Exact",
            "solo -> q/c Exact",
        ]
    );
}

/// Many packages with many dependencies must stay fast: name lookups are
/// indexed, not scanned.
#[test]
fn many_packages_stay_fast() {
    let mut entries: Vec<(String, String)> =
        vec![("package.json".into(), r#"{ "workspaces": ["p/*"] }"#.into())];
    for i in 0..2_000 {
        let deps: Vec<String> = (0..50)
            .map(|j| format!("\"p{}\": \"^1\"", (i + j) % 2_000))
            .collect();
        entries.push((
            format!("p/p{i}/package.json"),
            format!(
                "{{ \"name\": \"p{i}\", \"dependencies\": {{ {} }} }}",
                deps.join(", ")
            ),
        ));
    }
    let borrowed: Vec<(&str, &str)> = entries
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let start = std::time::Instant::now();
    let (model, _) = run(&files(&borrowed));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "took {:?}",
        start.elapsed()
    );
    assert_eq!(model.edges().count(), 2_000 * 49);
}
