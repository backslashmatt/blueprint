//! `bp model` against a fixture repository built at test time, with the JSON
//! snapshotted in `tests/snapshots/`. Set `UPDATE_SNAPSHOTS=1` to rewrite.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The fixture's files that a commit would hold.
const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n"),
    (
        "crates/app/Cargo.toml",
        "[package]\nname = \"app\"\n[dependencies]\ncore = { path = \"../core\" }\n",
    ),
    (
        "crates/app/src/main.rs",
        "mod cli;\nuse core::graph::Graph;\nfn main() {}\n",
    ),
    (
        "crates/app/src/cli.rs",
        "pub struct Args {\n    pub verbose: bool,\n}\n\nimpl Args {\n    pub fn parse() -> Self {\n        Args { verbose: false }\n    }\n}\n",
    ),
    ("crates/core/Cargo.toml", "[package]\nname = \"core\"\n"),
    (
        "crates/core/src/lib.rs",
        "pub mod graph;\npub(crate) mod util;\n",
    ),
    (
        "crates/core/src/graph.rs",
        "pub trait Walk {}\npub struct Graph;\nimpl Walk for Graph {}\n",
    ),
    ("crates/core/src/util.rs", ""),
    ("web/package.json", r#"{ "name": "web" }"#),
    ("docs/guide.md", "# guide\n"),
];

fn fixture(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bp-model-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let skipped: &[(&str, &str)] = &[
        // Skipped: version control and build output.
        (".git/HEAD", "ref: refs/heads/main\n"),
        (
            "target/debug/junk/Cargo.toml",
            "[package]\nname = \"junk\"\n",
        ),
        ("web/node_modules/dep/package.json", r#"{ "name": "dep" }"#),
    ];
    for (path, body) in FILES.iter().chain(skipped) {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    dir
}

/// The cache every test uses, so nothing is written to the user's cache.
fn cache() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("bp-cache-{}", std::process::id()))
}

fn bp(args: &[&str]) -> Output {
    bp_with_cache(args, &cache())
}

fn bp_with_cache(args: &[&str], cache: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bp"))
        .args(args)
        .env("BLUEPRINT_CACHE_DIR", cache)
        .output()
        .expect("run bp")
}

fn snapshot(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| {
            panic!(
                "no snapshot {}; run with UPDATE_SNAPSHOTS=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "snapshot {name} differs; run with UPDATE_SNAPSHOTS=1 if intended"
    );
}

#[test]
fn model_json_and_text_are_deterministic_and_snapshotted() {
    let dir = fixture("snapshot");
    let d = dir.to_str().unwrap();
    let json = bp(&["model", d, "--json"]);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let json = String::from_utf8(json.stdout).unwrap();
    snapshot("model_fixture.json", &json);
    assert_eq!(
        json,
        String::from_utf8(bp(&["model", d, "--json"]).stdout).unwrap(),
        "same input, same bytes"
    );
    // The JSON is a valid model.
    serde_json::from_str::<blueprint_model::Model>(&json).unwrap();
    assert!(
        !json.contains("junk") && !json.contains("\"dep\""),
        "target/ and node_modules/ are skipped"
    );

    let l1 = String::from_utf8(bp(&["model", d, "--level", "1"]).stdout).unwrap();
    snapshot("model_fixture_l1.txt", &l1);
    assert!(l1.lines().all(|l| l.starts_with("L1:")), "{l1}");
}

#[test]
fn missing_directory_fails_with_exit_1_and_bad_usage_with_exit_2() {
    let out = bp(&["model", "/no/such/directory/for/bp"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot read"));
    let out = bp(&["model", ".", "--level", "7"]);
    assert_eq!(out.status.code(), Some(2));
}

/// Writes the tree for `files` (path, blob id) and returns its id.
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

/// A git repository whose first commit holds only `crates/core` and whose
/// second (`HEAD`) holds the whole fixture. The working tree on disk holds
/// something else entirely, which `--rev` must ignore.
fn git_fixture(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bp-model-git-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = gix::init(&dir).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"worktree-only\"\n",
    )
    .unwrap();
    let blobs: Vec<(&str, gix::ObjectId)> = FILES
        .iter()
        .map(|(p, body)| (*p, repo.write_blob(body.as_bytes()).unwrap().detach()))
        .collect();
    let sig = gix::actor::SignatureRef {
        name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        time: "1700000000 +0000",
    };
    let core: Vec<_> = blobs
        .iter()
        .copied()
        .filter(|(p, _)| p.starts_with("crates/core/"))
        .collect();
    let first = repo
        .commit_as(
            sig,
            sig,
            "HEAD",
            "core",
            write_tree(&repo, &core),
            None::<gix::ObjectId>,
        )
        .unwrap()
        .detach();
    repo.commit_as(sig, sig, "HEAD", "all", write_tree(&repo, &blobs), [first])
        .unwrap();
    dir
}

#[test]
fn rev_reads_the_commit_not_the_working_tree() {
    let dir = git_fixture("rev");
    let d = dir.to_str().unwrap();
    let out = bp(&["model", d, "--rev", "HEAD", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The commit holds the same files as the directory fixture, so the model
    // is byte for byte the directory snapshot.
    snapshot(
        "model_fixture.json",
        &String::from_utf8(out.stdout).unwrap(),
    );

    let first =
        String::from_utf8(bp(&["model", d, "--rev", "HEAD~1", "--level", "1"]).stdout).unwrap();
    assert_eq!(first, "L1:cargo:crates/core#\tsystem\tcore\texact\n");
    let worktree = String::from_utf8(bp(&["model", d, "--level", "1"]).stdout).unwrap();
    assert!(worktree.contains("worktree-only"), "{worktree}");
}

#[test]
fn rev_that_does_not_resolve_fails_with_exit_1() {
    let dir = git_fixture("bad-rev");
    let out = bp(&["model", dir.to_str().unwrap(), "--rev", "no-such-branch"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no-such-branch"));
    let out = bp(&["model", "/no/such/repository/for/bp", "--rev", "HEAD"]);
    assert_eq!(out.status.code(), Some(1));
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// Deletes the loose object holding `spec` (e.g. `HEAD:path`), so that only
/// the index can still supply what was analysed from it.
fn delete_object(repo: &Path, spec: &str) {
    let r = gix::open(repo).unwrap();
    let id = r.rev_parse_single(spec).unwrap().detach().to_string();
    let file = repo.join(".git/objects").join(&id[..2]).join(&id[2..]);
    std::fs::remove_file(&file).unwrap();
}

#[test]
fn index_then_model_reads_the_index_not_the_objects() {
    let dir = git_fixture("index");
    let d = dir.to_str().unwrap();
    let cache = cache().join("index-test");
    let _ = std::fs::remove_dir_all(&cache);

    let first = stdout(&bp_with_cache(&["index", d], &cache));
    assert!(first.ends_with(" indexed (8 nodes)\n"), "{first}");
    let again = stdout(&bp_with_cache(&["index", d, "--rev", "HEAD"], &cache));
    assert!(again.ends_with(" already indexed (8 nodes)\n"), "{again}");
    let index_dirs = std::fs::read_dir(cache.join("index")).unwrap().count();
    assert_eq!(index_dirs, 1, "one index directory per repository");

    // Without the app's manifest the analysers would find a different model,
    // so a snapshot match means the model came from the index.
    delete_object(&dir, "HEAD:crates/app/Cargo.toml");
    let json = stdout(&bp_with_cache(
        &["model", d, "--rev", "HEAD", "--json"],
        &cache,
    ));
    snapshot("model_fixture.json", &json);
    let analysed = stdout(&bp_with_cache(
        &["model", d, "--rev", "HEAD", "--json"],
        &cache.join("elsewhere"),
    ));
    assert_ne!(analysed, json, "a fresh cache analyses the damaged commit");
}

#[test]
fn a_damaged_index_is_dropped_and_rebuilt() {
    let dir = git_fixture("damaged");
    let d = dir.to_str().unwrap();
    let cache = cache().join("damaged-test");
    let _ = std::fs::remove_dir_all(&cache);
    stdout(&bp_with_cache(&["index", d], &cache));
    let store = std::fs::read_dir(cache.join("index"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    for seg in std::fs::read_dir(&store).unwrap() {
        let seg = seg.unwrap().path();
        let mut bytes = std::fs::read(&seg).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&seg, bytes).unwrap();
    }
    let out = bp_with_cache(&["model", d, "--rev", "HEAD", "--json"], &cache);
    snapshot("model_fixture.json", &stdout(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("dropped and will be rebuilt"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rebuilt = stdout(&bp_with_cache(&["index", d], &cache));
    assert!(rebuilt.ends_with(" indexed (8 nodes)\n"), "{rebuilt}");
}

#[test]
fn index_fails_with_exit_1_when_the_revision_does_not_resolve() {
    let dir = git_fixture("index-bad-rev");
    let out = bp(&["index", dir.to_str().unwrap(), "--rev", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(bp(&["index"]).status.code(), Some(2));
}

#[test]
fn a_commit_with_unreadable_files_is_reported_and_never_indexed() {
    let dir = git_fixture("partial");
    let d = dir.to_str().unwrap();
    let cache = cache().join("partial-test");
    let _ = std::fs::remove_dir_all(&cache);

    // `bp model --rev` with no index yet creates nothing in the cache.
    stdout(&bp_with_cache(&["model", d, "--rev", "HEAD"], &cache));
    assert!(!cache.exists(), "bp model must not create an index");

    delete_object(&dir, "HEAD:crates/app/Cargo.toml");
    let out = bp_with_cache(&["index", d], &cache);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("crates/app/Cargo.toml: the file's object could not be read"),
        "{err}"
    );
    assert!(err.contains("not indexed"), "{err}");
    let again = bp_with_cache(&["index", d], &cache);
    assert_eq!(again.status.code(), Some(1), "nothing was stored to find");
    let deep = bp_with_cache(&["index", d, "--level", "4"], &cache);
    assert_eq!(deep.status.code(), Some(1), "level 4 fails the same way");
    assert!(deep.stdout.is_empty(), "no success line");
}

#[cfg(target_os = "linux")]
#[test]
fn peak_rss_is_reported_only_when_asked() {
    let out = Command::new(env!("CARGO_BIN_EXE_bp"))
        .arg("--version")
        .env("BLUEPRINT_REPORT_PEAK_RSS", "1")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let kb: u64 = err
        .lines()
        .find_map(|l| l.strip_prefix("peak_rss_kb: "))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no peak_rss_kb in {err:?}"));
    assert!(kb > 0);
    assert!(bp(&["--version"]).stderr.is_empty());
}

#[test]
fn level_4_reads_types_and_functions_the_same_from_disk_and_from_git() {
    let dir = git_fixture("level4");
    let d = dir.to_str().unwrap();
    let text = stdout(&bp(&["model", d, "--rev", "HEAD", "--level", "4"]));
    snapshot("model_fixture_l4.txt", &text);
    assert!(text.lines().any(|l| l.starts_with("L4:")), "{text}");
    let disk = stdout(&bp(&[
        "model",
        fixture("level4").to_str().unwrap(),
        "--level",
        "4",
    ]));
    assert_eq!(
        disk, text,
        "the working tree and the commit hold the same files"
    );
}

#[test]
fn every_level_keeps_the_system_edges() {
    let dir = fixture("levels");
    let d = dir.to_str().unwrap();
    let l1_edges = |level: &str| -> Vec<String> {
        stdout(&bp(&["model", d, "--level", level]))
            .lines()
            .filter(|l| l.starts_with("L1:") && l.contains(" -> "))
            .map(str::to_owned)
            .collect()
    };
    let at_1 = l1_edges("1");
    assert!(!at_1.is_empty());
    for level in ["2", "3", "4"] {
        assert_eq!(l1_edges(level), at_1, "level {level}");
    }
}

#[test]
fn index_level_4_parses_each_blob_once_and_model_reuses_the_facts() {
    let dir = git_fixture("facts");
    let d = dir.to_str().unwrap();
    let cache = cache().join("facts-test");
    let _ = std::fs::remove_dir_all(&cache);

    // The first commit holds core's three Rust files; HEAD adds app's two.
    let first = stdout(&bp_with_cache(
        &["index", d, "--rev", "HEAD~1", "--level", "4"],
        &cache,
    ));
    assert!(first.ends_with("; 3 files parsed, 0 reused)\n"), "{first}");
    let head = stdout(&bp_with_cache(&["index", d, "--level", "4"], &cache));
    assert!(head.ends_with("; 2 files parsed, 3 reused)\n"), "{head}");
    let again = stdout(&bp_with_cache(&["index", d, "--level", "4"], &cache));
    assert!(again.ends_with("; 0 files parsed, 5 reused)\n"), "{again}");

    // Facts from the index build the same model as parsing.
    let reused = stdout(&bp_with_cache(
        &["model", d, "--rev", "HEAD", "--level", "4"],
        &cache,
    ));
    snapshot("model_fixture_l4.txt", &reused);
    // Level 2 indexing still stores nothing deeper.
    let shallow = stdout(&bp_with_cache(&["index", d], &cache.join("l2")));
    assert!(shallow.ends_with(" indexed (8 nodes)\n"), "{shallow}");
}

#[test]
fn diff_reports_what_the_second_commit_added() {
    let dir = git_fixture("diff");
    let d = dir.to_str().unwrap();
    let text = stdout(&bp(&["diff", d, "HEAD~1", "HEAD"]));
    // HEAD adds the app crate, which depends on core.
    assert!(text.contains("added\tL1:cargo:crates/app#\n"), "{text}");
    assert!(
        text.contains(
            "finding\tnew-dependency\tL1:cargo:crates/app# -> L1:cargo:crates/core#\tdepends-on\n"
        ),
        "{text}"
    );
    let json = stdout(&bp(&["diff", d, "HEAD~1", "HEAD", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        v["findings"].as_array().is_some_and(|f| !f.is_empty()),
        "{json}"
    );
    assert_eq!(
        stdout(&bp(&["diff", d, "HEAD~1", "HEAD"])),
        text,
        "deterministic"
    );
    assert_eq!(stdout(&bp(&["diff", d, "HEAD", "HEAD"])), "", "no change");
    let bad = bp(&["diff", d, "HEAD", "no-such-rev"]);
    assert_eq!(bad.status.code(), Some(1));
}
