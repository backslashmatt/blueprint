//! Tests for the git tree reader against fixture repositories built here,
//! with `gix` writing objects directly (no `git` binary, no network).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use blueprint_engine::git::{EntryKind, GitError, ObjectId, Repo};
use blueprint_model::Confidence;
use gix::bstr::BString;
use gix::objs::tree::{Entry, EntryKind as K};
use gix::refs::transaction::PreviousValue;

type R<T> = Result<T, Box<dyn std::error::Error>>;

fn fresh_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("git-{name}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn sig() -> gix::actor::SignatureRef<'static> {
    gix::actor::SignatureRef {
        name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        time: "1700000000 +0000",
    }
}

fn tree(repo: &gix::Repository, mut entries: Vec<(&[u8], K, ObjectId)>) -> R<ObjectId> {
    let mut t = gix::objs::Tree::empty();
    for (name, kind, oid) in entries.drain(..) {
        t.entries.push(Entry {
            mode: kind.into(),
            filename: BString::from(name),
            oid,
        });
    }
    t.entries.sort();
    Ok(repo.write_object(&t)?.detach())
}

struct Fixture {
    dir: PathBuf,
    first: ObjectId,
    second: ObjectId,
    readme: ObjectId,
    submodule_commit: ObjectId,
}

/// Two commits on `main`. The second has nested directories, an executable,
/// a symlink, a submodule and a file whose name is not UTF-8. The first is
/// tagged `v1` with an annotated tag.
fn fixture() -> R<Fixture> {
    let dir = fresh_dir("fixture");
    let repo = gix::init_bare(&dir)?;
    // Pin HEAD: `init.defaultBranch` in the runner's config must not matter.
    std::fs::write(dir.join("HEAD"), "ref: refs/heads/main\n")?;
    let readme = repo.write_blob(b"# readme\n")?.detach();
    let first_tree = tree(&repo, vec![(b"README.md", K::Blob, readme)])?;
    let first = repo
        .commit_as(
            sig(),
            sig(),
            "refs/heads/main",
            "first",
            first_tree,
            None::<ObjectId>,
        )?
        .detach();

    let lib = repo.write_blob(b"pub fn f() {}\n")?.detach();
    let script = repo.write_blob(b"#!/bin/sh\n")?.detach();
    let link = repo.write_blob(b"../outside/target")?.detach();
    let latin1 = repo.write_blob(b"bytes")?.detach();
    let submodule_commit = ObjectId::from_hex(b"1111111111111111111111111111111111111111")?;
    let src = tree(
        &repo,
        vec![(b"lib.rs", K::Blob, lib), (b"caf\xe9.txt", K::Blob, latin1)],
    )?;
    let crate_dir = tree(&repo, vec![(b"src", K::Tree, src)])?;
    let root = tree(
        &repo,
        vec![
            (b"README.md", K::Blob, readme),
            (b"crates", K::Tree, crate_dir),
            (b"run.sh", K::BlobExecutable, script),
            (b"link", K::Link, link),
            (b"vendor", K::Commit, submodule_commit),
        ],
    )?;
    let second = repo
        .commit_as(sig(), sig(), "refs/heads/main", "second", root, [first])?
        .detach();
    repo.tag(
        "v1",
        first,
        gix::objs::Kind::Commit,
        Some(sig()),
        "release",
        PreviousValue::MustNotExist,
    )?;
    Ok(Fixture {
        dir,
        first,
        second,
        readme,
        submodule_commit,
    })
}

#[test]
fn resolves_branches_tags_ids_and_head() -> R<()> {
    let f = fixture()?;
    let repo = Repo::open(&f.dir)?;
    assert_eq!(repo.resolve("main")?, f.second);
    assert_eq!(repo.resolve("refs/heads/main")?, f.second);
    assert_eq!(
        repo.resolve("HEAD")?,
        f.second,
        "a fresh repository's HEAD points at main"
    );
    assert_eq!(repo.resolve("main~1")?, f.first);
    assert_eq!(
        repo.resolve("v1")?,
        f.first,
        "annotated tags are peeled to the commit"
    );
    assert_eq!(repo.resolve(&f.second.to_string())?, f.second);
    assert_eq!(repo.resolve(&f.second.to_string()[..10])?, f.second);
    Ok(())
}

#[test]
fn unknown_revisions_and_non_commits_are_errors() -> R<()> {
    let f = fixture()?;
    let repo = Repo::open(&f.dir)?;
    assert!(matches!(
        repo.resolve("no-such-branch"),
        Err(GitError::Resolve { .. })
    ));
    assert!(
        matches!(
            repo.resolve(&f.readme.to_string()),
            Err(GitError::Resolve { .. })
        ),
        "a blob is not a commit"
    );
    assert!(matches!(
        Repo::open(&fresh_dir("missing")),
        Err(GitError::Open(_))
    ));
    Ok(())
}

#[test]
fn lists_the_tree_with_kinds_ids_and_raw_paths() -> R<()> {
    let f = fixture()?;
    let repo = Repo::open(&f.dir)?;
    let entries = repo.tree(f.second)?;
    let listed: Vec<(String, EntryKind)> =
        entries.iter().map(|e| (e.path_lossy(), e.kind)).collect();
    assert_eq!(
        listed,
        vec![
            ("README.md".to_owned(), EntryKind::Blob),
            ("crates/src/caf\u{fffd}.txt".to_owned(), EntryKind::Blob),
            ("crates/src/lib.rs".to_owned(), EntryKind::Blob),
            ("link".to_owned(), EntryKind::Symlink),
            ("run.sh".to_owned(), EntryKind::Executable),
            ("vendor".to_owned(), EntryKind::Submodule),
        ]
    );
    let latin1 = &entries[1];
    assert_eq!(latin1.path, b"crates/src/caf\xe9.txt");
    assert_eq!(
        latin1.path_str(),
        None,
        "non-UTF-8 paths are kept as bytes, not rejected"
    );
    assert_eq!(entries[0].id, f.readme, "the blob id is the content hash");
    assert_eq!(repo.tree(f.first)?.len(), 1);
    Ok(())
}

#[test]
fn symlinks_are_recorded_not_followed_and_submodules_are_unknown() -> R<()> {
    let f = fixture()?;
    let repo = Repo::open(&f.dir)?;
    let entries = repo.tree(f.second)?;
    let link = entries
        .iter()
        .find(|e| e.kind == EntryKind::Symlink)
        .ok_or("no symlink")?;
    assert_eq!(repo.blob(link.id)?, b"../outside/target");
    let sub = entries
        .iter()
        .find(|e| e.kind == EntryKind::Submodule)
        .ok_or("no submodule")?;
    assert_eq!(sub.id, f.submodule_commit);
    assert_eq!(sub.kind.confidence(), Confidence::Unknown);
    assert_eq!(EntryKind::Blob.confidence(), Confidence::Exact);
    assert!(
        matches!(repo.blob(sub.id), Err(GitError::Read(_))),
        "the submodule's commit is not in this repository"
    );
    Ok(())
}

#[test]
fn reads_blobs_and_refuses_other_objects() -> R<()> {
    let f = fixture()?;
    let repo = Repo::open(&f.dir)?;
    assert_eq!(repo.blob(f.readme)?, b"# readme\n");
    assert!(
        matches!(repo.blob(f.first), Err(GitError::Read(_))),
        "a commit is not a blob"
    );
    Ok(())
}
