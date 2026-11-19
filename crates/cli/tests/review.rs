//! `bp review` (#115): marks stored per repository, rolled up, kept across
//! a rebase that leaves a node's change alone, and cleared when it does not.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// One commit per entry of `libs`, each with that `src/lib.rs`.
fn repo(name: &str, libs: &[&str]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bp-review-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = gix::init(&dir).unwrap();
    let sig = gix::actor::SignatureRef {
        name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        time: "1700000000 +0000",
    };
    let mut parent: Option<gix::ObjectId> = None;
    for lib in libs {
        let files = [
            ("Cargo.toml", "[package]\nname = \"app\"\n"),
            ("src/lib.rs", lib),
        ];
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

/// Runs `bp review <dir> <base> <head> <extra>`; its stdout lines, or panics.
fn bp_review(dir: &Path, base: &str, head: &str, extra: &[&str]) -> Vec<String> {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let out = Command::new(env!("CARGO_BIN_EXE_bp"))
        .args(["review", dir.to_str().unwrap(), base, head])
        .args(extra)
        .env("BLUEPRINT_CACHE_DIR", tmp.join("bp-review-cache"))
        .env(
            "BLUEPRINT_DATA_DIR",
            tmp.join(format!("bp-review-data-{}", std::process::id())),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn status<'a>(lines: &'a [String], node: &str) -> &'a str {
    let line = lines.iter().find(|l| l.split('\t').nth(1) == Some(node));
    line.and_then(|l| l.split('\t').next())
        .unwrap_or_else(|| panic!("{node}: {lines:?}"))
}

const F: &str = "L4:cargo:#(functions)/f";
const G: &str = "L4:cargo:#(functions)/g";
const SYSTEM: &str = "L1:cargo:#";

const BASE: &str = "pub fn f() -> u8 {\n    1\n}\n\npub fn g() -> u8 {\n    1\n}\n";
const HEAD: &str = "pub fn f() -> u8 {\n    2\n}\n\npub fn g() -> u8 {\n    2\n}\n";
/// Main moved on: a line above both functions.
const MAIN: &str = "// app\npub fn f() -> u8 {\n    1\n}\n\npub fn g() -> u8 {\n    1\n}\n";
/// The change rebased onto it.
const REBASED: &str = "// app\npub fn f() -> u8 {\n    2\n}\n\npub fn g() -> u8 {\n    2\n}\n";
/// Rebased again, with g's change different.
const REBASED_G3: &str = "// app\npub fn f() -> u8 {\n    2\n}\n\npub fn g() -> u8 {\n    3\n}\n";

#[test]
fn marks_roll_up_persist_and_survive_a_rebase_that_keeps_the_change() {
    let dir = repo("rebase", &[BASE, HEAD, MAIN, REBASED, REBASED_G3]);
    let (base, head) = ("HEAD~4", "HEAD~3");
    let blank = bp_review(&dir, base, head, &[]);
    assert_eq!(blank[0], "reviewed 0 of 2", "{blank:?}");
    assert_eq!(status(&blank, F), "open");

    let once = bp_review(&dir, base, head, &["--mark", F, "--note", "bounds checked"]);
    assert_eq!(once[0], "reviewed 1 of 2");
    let f_line = once.iter().find(|l| l.contains(F)).unwrap();
    assert!(f_line.ends_with("\tbounds checked"), "{f_line}");
    assert_eq!(status(&once, SYSTEM), "open", "g is still open");

    let both = bp_review(&dir, base, head, &["--mark", G]);
    assert_eq!(both[0], "reviewed 2 of 2");
    assert_eq!(status(&both, SYSTEM), "reviewed", "rolls up to the system");

    // The same change on a new base: both marks hold.
    let rebased = bp_review(&dir, "HEAD~2", "HEAD~1", &[]);
    assert_eq!(rebased[0], "reviewed 2 of 2", "{rebased:?}");

    // g's change differs from what was reviewed: only f's mark holds.
    let changed = bp_review(&dir, "HEAD~2", "HEAD", &[]);
    assert_eq!(status(&changed, F), "reviewed", "{changed:?}");
    assert_eq!(status(&changed, G), "open");
    assert_eq!(status(&changed, SYSTEM), "open");

    // Unmarking keeps the note.
    let unmarked = bp_review(&dir, base, head, &["--unmark", F]);
    assert_eq!(status(&unmarked, F), "open");
    assert!(
        unmarked
            .iter()
            .any(|l| l.contains(F) && l.ends_with("bounds checked"))
    );
}

#[test]
fn marking_a_node_that_did_not_change_fails() {
    let dir = repo("unchanged", &[BASE, HEAD]);
    let out = Command::new(env!("CARGO_BIN_EXE_bp"))
        .args([
            "review",
            dir.to_str().unwrap(),
            "HEAD~1",
            "HEAD",
            "--mark",
            "L1:cargo:#x",
        ])
        .env(
            "BLUEPRINT_CACHE_DIR",
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bp-review-cache"),
        )
        .env(
            "BLUEPRINT_DATA_DIR",
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bp-review-data-unchanged"),
        )
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("not a node id") || err.contains("did not change"),
        "{err}"
    );
}
