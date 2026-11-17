//! L1 Go analysis over fixtures, as text snapshots in `tests/snapshots/`.
//! Set `UPDATE_SNAPSHOTS=1` to rewrite them.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{Diagnostic, go};
use blueprint_model::{Confidence, Model, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let mut model = Model::default();
    let diags = go::analyse(&paths, &|p| files.get(p).cloned(), &mut model);
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
fn go_work_replace_and_name_matches() {
    let entries = [
        (
            "go.work",
            "go 1.22\n\nuse (\n\t./api\n\t./svc // the service\n)\nuse ./tools\n",
        ),
        (
            "api/go.mod",
            "module example.com/api\n\ngo 1.22\n\nrequire (\n\texample.com/shared v0.1.0\n\tgolang.org/x/text v0.3.0 // indirect\n)\n",
        ),
        (
            "svc/go.mod",
            "module example.com/svc\n\nrequire example.com/api v0.0.0\nrequire \"example.com/lib\" v1.0.0\nrequire example.com/tools v0.0.0 // indirect\n\nreplace example.com/lib v1.0.0 => ../third_party/lib\n",
        ),
        ("tools/go.mod", "module example.com/tools\n"),
        (
            "shared/go.mod",
            "// outside go.work\nmodule example.com/shared\n",
        ),
        ("third_party/lib/go.mod", "module example.com/lib\n"),
    ];
    let (model, diags) = run(&files(&entries));
    snapshot("l1_go_work", &render(&model, &diags));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        edges(&model),
        vec![
            "api -> shared Inferred",
            "svc -> api Exact",
            "svc -> third_party/lib Exact",
        ],
        "go.work and local replace are exact, a bare path match is inferred, indirect requires are skipped"
    );
}

#[test]
fn malformed_and_dangling() {
    let (model, diags) = run(&files(&[
        (
            "a/go.mod",
            "module example.com/a\nreplace example.com/b => ../nowhere\nrequire example.com/b v1\n",
        ),
        ("b/go.mod", "go 1.22\n"),
        ("c/go.mod", "\u{0}\u{1}not go at all ((("),
    ]));
    snapshot("l1_go_malformed", &render(&model, &diags));
    let unknown: Vec<String> = model
        .nodes()
        .filter(|n| n.confidence == Confidence::Unknown)
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(unknown, vec!["b", "c"]);
    assert!(
        diags.iter().any(|d| d.message.contains("has no go.mod")),
        "{diags:?}"
    );
}

#[test]
fn mutated_manifests_never_panic() {
    let seed = files(&[
        ("go.work", "use (\n ./a\n ./b\n)\n"),
        (
            "a/go.mod",
            "module x/a\nrequire (\n x/b v1\n)\nreplace x/b => ../b\n",
        ),
        ("b/go.mod", "module x/b\n"),
    ]);
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tokens: [&[u8]; 8] = [
        b"(",
        b")",
        b"=>",
        b"//",
        b"\"",
        b"../../..",
        b"\n",
        b"require ",
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
fn review_findings_replace_rules_ignored_dirs_and_syntax() {
    let entries = [
        ("go.work", "use ./w\nreplace x/over => ./over2\n"),
        (
            "w/go.mod",
            "module x/w\nrequire x/over v1\nreplace x/over => ../over\n",
        ),
        ("over/go.mod", "module x/over\n"),
        ("over2/go.mod", "module x/over\n"),
        // require( without a space; a remote replace stops name matching;
        // a versioned replace applies only to that version; `..` is local.
        (
            "a/go.mod",
            "module x/a\nrequire(\n\tx/b v1\n\tx/c v1.1.0\n\tx/root v0\n\tx/d v1 // indirect; kept for tools\n)\nreplace x/b => github.com/fork/b v1.2.0\nreplace x/c v1.0.0 => ../c\nreplace x/root => ..\n",
        ),
        ("b/go.mod", "module x/b\n"),
        ("c/go.mod", "module x/c\n"),
        ("d/go.mod", "module x/d\n"),
        ("go.mod", "module x/rootreal\n"),
        // The go tool ignores these.
        ("b/testdata/fake/go.mod", "module x/b\n"),
        ("vendor/x/c/go.mod", "module x/c\n"),
        ("_old/go.mod", "module x/c\n"),
    ];
    let (model, diags) = run(&files(&entries));
    snapshot("l1_go_replace", &render(&model, &diags));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        edges(&model),
        vec!["a ->  Exact", "a -> c Inferred", "w -> over2 Exact"],
        "go.work replace wins; remote replace means no edge; versioned replace only for its version"
    );
    assert!(!model.nodes().any(|n| {
        ["b/testdata/fake", "vendor/x/c", "_old"]
            .contains(&n.id.system_key().dir.to_string().as_str())
    }));
}

#[test]
fn go_work_problems_are_reported() {
    let (_, diags) = run(&files(&[
        ("go.work", "use (\n ./missing\n"),
        ("bad/go.work", "\u{ff}"),
    ]));
    let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(messages.contains(&"block not closed"), "{messages:?}");
    assert!(
        messages.iter().any(|m| m.contains("has no go.mod")),
        "{messages:?}"
    );
}
