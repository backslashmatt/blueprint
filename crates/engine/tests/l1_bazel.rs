//! L1 Bazel analysis over fixtures, as text snapshots in `tests/snapshots/`.
//! Set `UPDATE_SNAPSHOTS=1` to rewrite them.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{Diagnostic, bazel};
use blueprint_model::{Confidence, Model, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let mut model = Model::default();
    let diags = bazel::analyse(&paths, &|p| files.get(p).cloned(), &mut model);
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
fn labels_in_dependency_attributes() {
    let (model, diags) = run(&files(&[
        ("WORKSPACE", ""),
        (
            "app/BUILD.bazel",
            r#"load("//tools:defs.bzl", "my_macro")  # load paths are not dependencies
cc_binary(
    name = "app",
    srcs = ["main.cc", "//gen:sources"],
    deps = [
        ":local",
        "//lib/core",
        "@@//lib/net:net",
        "@abseil//absl/strings",  # another repository
    ] + select({
        "//conditions:default": ["//lib/plat:posix"],
        "@platforms//os:windows": ["//lib/plat:win"],
    }),
    visibility = ["//visibility:public"],
    tags = ["//not/a/dep"],
)
"#,
        ),
        ("app/BUILD", "this file is shadowed by BUILD.bazel"),
        (
            "gen/BUILD",
            "genrule(name = 'sources', srcs = [], outs = ['a.cc'], cmd = '')\n",
        ),
        (
            "lib/core/BUILD.bazel",
            "cc_library(name = 'core', deps = [\"//lib/missing\"])\n",
        ),
        ("lib/net/BUILD", ""),
        ("lib/plat/BUILD", ""),
        ("tools/BUILD", ""),
        (
            "bad/BUILD",
            "cc_library(name = \"x\", deps = [\"//lib/core\"\n",
        ),
    ]));
    snapshot("l1_bazel", &render(&model, &diags));
    assert_eq!(
        edges(&model),
        vec![
            "app -> gen Exact",
            "app -> lib/core Exact",
            "app -> lib/net Exact",
            "app -> lib/plat Exact",
            "bad -> lib/core Inferred"
        ],
        "main-repo labels in dependency attributes, including select() values, but not keys, loads, visibility or tags"
    );
    let unknown: Vec<String> = model
        .nodes()
        .filter(|n| n.confidence == Confidence::Unknown)
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(unknown, vec!["//bad"]);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("//lib/missing names no package")),
        "{diags:?}"
    );
}

#[test]
fn mutated_build_files_never_panic() {
    let seed = files(&[
        (
            "a/BUILD",
            "x(name = 'a', deps = ['//b', \"//c:t\"] + select({'//k': ['//b']}))\n",
        ),
        ("b/BUILD", "y(srcs = [r'raw\\str', '''tri\nple'''])\n"),
        ("c/BUILD.bazel", "# comment\n"),
    ]);
    let mut state = 0x7777_1111_2222_3333u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tokens: [&[u8]; 10] = [
        b"(", b")", b"[", b"]", b"'", b"\"\"\"", b"deps =", b"//../..", b"#", b"\\",
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
fn review_findings_joins_top_level_repos_and_bzlmod() {
    let (model, diags) = run(&files(&[
        (
            "MODULE.bazel",
            "module(name = \"acme\", version = \"1.0\")\n",
        ),
        (".bazelignore", "# generated\nignored\n"),
        (
            "a/BUILD",
            r#"load("//tools:defs.bzl", "m")
X = ["//b" + "/c:x", "//b/" + NAME, "//b:%s" % NAME, "//b:{}".format(NAME)]
deps = ["//top_level_is_not_an_attribute"]
cc_library(name = "a", deps = ["@acme//lib", "//b/c", "//tools" + ":gen", "//vis:%s" % N], visibility = ["//vis:__subpackages__"])
"#,
        ),
        ("b/BUILD", ""),
        ("b/c/BUILD", ""),
        ("lib/BUILD", ""),
        ("tools/BUILD", ""),
        ("vis/BUILD", ""),
        ("ignored/BUILD", "x(deps = ['//lib'])"),
        // A nested repository: its // labels resolve inside it.
        ("third_party/sub/WORKSPACE", ""),
        ("third_party/sub/BUILD", "x(deps = ['//lib'])"),
        ("third_party/sub/lib/BUILD", ""),
    ]));
    snapshot("l1_bazel_review", &render(&model, &diags));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        edges(&model),
        vec![
            "a -> b/c Exact",
            "a -> lib Exact",
            "third_party/sub -> third_party/sub/lib Exact"
        ],
        "joined strings, top-level assignments, loads and visibility make no edge"
    );
    assert!(
        !model
            .nodes()
            .any(|n| n.id.system_key().dir.to_string() == "ignored")
    );
    assert!(model.nodes().any(|n| n.name == "third_party/sub//lib"));
}
