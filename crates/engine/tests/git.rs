//! Tests for the git tree reader against fixture repositories built here,
//! with `gix` writing objects directly (no `git` binary, no network).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use blueprint_engine::git::{EntryKind, GitError, ObjectId, RefKind, Repo};
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
fn lists_refs_that_lead_to_commits() -> R<()> {
    let f = fixture()?;
    let git = gix::open(&f.dir)?;
    // A tag of a blob and a symbolic ref are not reviewable commits.
    git.tag_reference("blob-tag", f.readme, PreviousValue::MustNotExist)?;
    std::fs::create_dir_all(f.dir.join("refs/remotes/origin"))?;
    std::fs::write(
        f.dir.join("refs/remotes/origin/main"),
        format!("{}\n", f.first),
    )?;
    std::fs::create_dir_all(f.dir.join("refs/pull/7"))?;
    std::fs::write(f.dir.join("refs/pull/7/head"), format!("{}\n", f.second))?;
    std::fs::write(
        f.dir.join("refs/remotes/origin/HEAD"),
        "ref: refs/remotes/origin/main\n",
    )?;
    let refs = Repo::open(&f.dir)?.refs()?;
    let got: Vec<(&str, RefKind, ObjectId)> = refs
        .iter()
        .map(|r| (r.name.as_str(), r.kind, r.commit))
        .collect();
    assert_eq!(
        got,
        vec![
            ("refs/heads/main", RefKind::Branch, f.second),
            ("refs/pull/7/head", RefKind::PullRequest, f.second),
            ("refs/remotes/origin/main", RefKind::RemoteBranch, f.first),
            ("refs/tags/v1", RefKind::Tag, f.first),
        ]
    );
    Ok(())
}

#[test]
fn logs_commits_newest_first_with_their_details() -> R<()> {
    let f = fixture()?;
    let repo = Repo::open(&f.dir)?;
    assert_eq!(repo.head(), Some(f.second));
    let log = repo.log(f.second, 10)?;
    let ids: Vec<ObjectId> = log.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec![f.second, f.first]);
    assert_eq!(log[0].summary, "second");
    assert_eq!(log[0].author, "Fixture");
    assert_eq!(log[0].time, 1_700_000_000);
    assert_eq!(log[0].parents, vec![f.first]);
    assert!(log[1].parents.is_empty());
    assert_eq!(repo.log(f.second, 1)?.len(), 1, "the limit is respected");
    Ok(())
}

#[test]
fn suggests_the_branch_against_its_merge_base_with_the_default_branch() -> R<()> {
    let f = fixture()?;
    let git = gix::open(&f.dir)?;
    let topic_tree = tree(&git, vec![(b"TOPIC.md", K::Blob, f.readme)])?;
    let topic = git
        .commit_as(
            sig(),
            sig(),
            "refs/heads/topic",
            "topic",
            topic_tree,
            [f.second],
        )?
        .detach();
    let repo = Repo::open(&f.dir)?;
    assert_eq!(repo.head_branch().as_deref(), Some("refs/heads/main"));
    assert_eq!(
        repo.suggested_review(),
        Some((f.first, f.second)),
        "on main: HEAD against its parent"
    );
    std::fs::write(f.dir.join("HEAD"), "ref: refs/heads/topic\n")?;
    let repo = Repo::open(&f.dir)?;
    assert_eq!(repo.head_branch().as_deref(), Some("refs/heads/topic"));
    assert_eq!(
        repo.default_branch(),
        Some(("refs/heads/main".to_string(), f.second))
    );
    assert_eq!(repo.merge_base(topic, f.second)?, Some(f.second));
    assert_eq!(repo.suggested_review(), Some((f.second, topic)));
    // Detached HEAD: no branch, still a suggestion.
    std::fs::write(f.dir.join("HEAD"), format!("{topic}\n"))?;
    let repo = Repo::open(&f.dir)?;
    assert_eq!(repo.head_branch(), None);
    assert_eq!(repo.suggested_review(), Some((f.second, topic)));
    Ok(())
}

#[test]
fn an_empty_repository_has_no_head() -> R<()> {
    let dir = fresh_dir("empty");
    gix::init_bare(&dir)?;
    let repo = Repo::open(&dir)?;
    assert_eq!(repo.head(), None);
    assert!(repo.refs()?.is_empty());
    assert_eq!(repo.suggested_review(), None);
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

/// Writes a tree object byte for byte, so entries git would never write can
/// be tested. Each entry is `(mode, name, id)`, in the order given.
fn raw_tree(repo: &gix::Repository, entries: &[(&[u8], &[u8], ObjectId)]) -> R<ObjectId> {
    use gix::prelude::Write;
    let mut body = Vec::new();
    for (mode, name, oid) in entries {
        body.extend_from_slice(mode);
        body.push(b' ');
        body.extend_from_slice(name);
        body.push(0);
        body.extend_from_slice(oid.as_bytes());
    }
    Ok(repo.objects.write_buf(gix::objs::Kind::Tree, &body)?)
}

fn commit_tree(repo: &gix::Repository, tree: ObjectId) -> R<ObjectId> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let r = format!("refs/heads/t{}", N.fetch_add(1, Ordering::Relaxed));
    Ok(repo
        .commit_as(sig(), sig(), r.as_str(), "m", tree, None::<ObjectId>)?
        .detach())
}

fn read_error(r: Result<Vec<blueprint_engine::git::TreeEntry>, GitError>) -> String {
    match r {
        Err(GitError::Read(e)) => e,
        other => format!("expected a read error, got {other:?}"),
    }
}

#[test]
fn names_git_would_reject_are_errors_not_wrong_paths() -> R<()> {
    let dir = fresh_dir("names");
    let repo = gix::init_bare(&dir)?;
    let blob = repo.write_blob(b"x")?.detach();
    let sub = tree(&repo, vec![(b"b", K::Blob, blob)])?;
    let r = Repo::open(&dir)?;
    for bad in [&b""[..], b".", b"..", b"a/b"] {
        let t = raw_tree(&repo, &[(b"100644", bad, blob), (b"40000", b"a", sub)])?;
        let e = read_error(r.tree(commit_tree(&repo, t)?));
        assert!(e.contains("invalid entry name"), "{bad:?}: {e}");
    }
    Ok(())
}

#[test]
fn unknown_modes_are_errors_not_submodules() -> R<()> {
    let dir = fresh_dir("modes");
    let repo = gix::init_bare(&dir)?;
    let blob = repo.write_blob(b"x")?.detach();
    let r = Repo::open(&dir)?;
    for mode in [&b"100"[..], b"100600", b"170000"] {
        let t = raw_tree(&repo, &[(mode, b"f", blob)])?;
        let e = read_error(r.tree(commit_tree(&repo, t)?));
        assert!(e.contains("unknown mode"), "{mode:?}: {e}");
    }
    // The legacy group-writable mode and the zero-padded tree mode still read.
    let sub = raw_tree(&repo, &[(b"100664", b"g", blob)])?;
    let t = raw_tree(&repo, &[(b"040000", b"d", sub)])?;
    let got = r.tree(commit_tree(&repo, t)?)?;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, b"d/g");
    assert_eq!(got[0].kind, EntryKind::Blob);
    Ok(())
}

#[test]
fn shared_subtrees_cannot_expand_without_limit() -> R<()> {
    let dir = fresh_dir("bomb");
    let repo = gix::init_bare(&dir)?;
    let blob = repo.write_blob(b"x")?.detach();
    // 12 levels of {a: T, b: T}: 14 objects, 2^12 files, 2^13 - 2 + 2^12 entries.
    let mut t = tree(&repo, vec![(b"f", K::Blob, blob)])?;
    for _ in 0..12 {
        t = tree(&repo, vec![(b"a", K::Tree, t), (b"b", K::Tree, t)])?;
    }
    let c = commit_tree(&repo, t)?;
    let r = Repo::open(&dir)?;
    assert_eq!(r.tree(c)?.len(), 4096);
    let e = read_error(r.tree_limited(c, 1000));
    assert!(e.contains("more than 1000 entries"), "{e}");
    Ok(())
}

#[test]
fn commit_source_reads_files_and_reports_what_it_skips() -> R<()> {
    use blueprint_engine::source::Commit;
    let f = fixture()?;
    let (commit, diags) = Commit::open(&f.dir, "main")?;
    assert_eq!(commit.id(), f.second);
    let paths: Vec<String> = commit.paths().iter().map(ToString::to_string).collect();
    // The symlink is not followed; the submodule and the non-UTF-8 name are
    // reported instead of read.
    assert_eq!(paths, ["README.md", "crates/src/lib.rs", "run.sh"]);
    let messages: Vec<String> = diags
        .iter()
        .map(|d| format!("{}: {}", d.path, d.message))
        .collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages.iter().any(|m| m.contains("not UTF-8")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.starts_with("vendor: submodule")),
        "{messages:?}"
    );
    let lib = commit.paths()[1].clone();
    assert_eq!(commit.read(&lib).as_deref(), Some(&b"pub fn f() {}\n"[..]));
    let missing = blueprint_model::RepoPath::parse("nope.rs")?;
    assert_eq!(commit.read(&missing), None);
    assert!(Commit::open(&f.dir, "no-such-branch").is_err());
    Ok(())
}

#[test]
fn changed_blobs_reads_only_what_differs() -> R<()> {
    let dir = fresh_dir("changed");
    let repo = gix::init(&dir)?;
    let blob = |s: &str| -> R<ObjectId> { Ok(repo.write_blob(s.as_bytes())?.detach()) };
    let (same, old, new, gone, added) = (
        blob("same")?,
        blob("old")?,
        blob("new")?,
        blob("gone")?,
        blob("added")?,
    );
    // Base: keep/same, src/deep/f (old), src/gone, x/ (a directory).
    let deep_base = tree(&repo, vec![(b"f", K::Blob, old)])?;
    let x_dir = tree(&repo, vec![(b"inside", K::Blob, same)])?;
    let keep = tree(&repo, vec![(b"same", K::Blob, same)])?;
    let src_base = tree(
        &repo,
        vec![(b"deep", K::Tree, deep_base), (b"gone", K::Blob, gone)],
    )?;
    let base = tree(
        &repo,
        vec![
            (b"keep", K::Tree, keep),
            (b"src", K::Tree, src_base),
            (b"x", K::Tree, x_dir),
        ],
    )?;
    // Head: keep/ unchanged, src/deep/f (new), src/added, x is now a file.
    let deep_head = tree(&repo, vec![(b"f", K::Blob, new)])?;
    let src_head = tree(
        &repo,
        vec![(b"added", K::Blob, added), (b"deep", K::Tree, deep_head)],
    )?;
    let head = tree(
        &repo,
        vec![
            (b"keep", K::Tree, keep),
            (b"src", K::Tree, src_head),
            (b"x", K::Blob, added),
        ],
    )?;
    let (b, h) = (commit_tree(&repo, base)?, commit_tree(&repo, head)?);
    let r = Repo::open(&dir)?;
    let got: Vec<(String, Option<ObjectId>, Option<ObjectId>)> = r
        .changed_blobs(b, h)?
        .into_iter()
        .map(|(p, x, y)| (String::from_utf8_lossy(&p).into_owned(), x, y))
        .collect();
    let want = vec![
        ("src/added".to_owned(), None, Some(added)),
        ("src/deep/f".to_owned(), Some(old), Some(new)),
        ("src/gone".to_owned(), Some(gone), None),
        ("x".to_owned(), None, Some(added)),
        ("x/inside".to_owned(), Some(same), None),
    ];
    assert_eq!(got, want);
    assert!(r.changed_blobs(h, h)?.is_empty());
    // Under one directory (#169): only its subtree is read, paths stay
    // whole, and a name that only starts the same is not inside it.
    let under = |dir: &[&str]| -> R<Vec<String>> {
        let dir: Vec<String> = dir.iter().map(|s| (*s).to_owned()).collect();
        Ok(r.changed_blobs_under(b, h, &dir)?
            .into_iter()
            .map(|(p, _, _)| String::from_utf8_lossy(&p).into_owned())
            .collect())
    };
    assert_eq!(under(&["src"])?, ["src/added", "src/deep/f", "src/gone"]);
    assert_eq!(under(&["src", "deep"])?, ["src/deep/f"]);
    assert_eq!(under(&["x"])?, ["x/inside"], "a directory only in base");
    assert!(under(&["keep"])?.is_empty(), "unchanged");
    assert!(under(&["sr"])?.is_empty(), "not a prefix match");
    assert!(under(&["nope"])?.is_empty());
    assert!(
        under(&["src", "added"])?.is_empty(),
        "a file is no directory"
    );
    assert_eq!(under(&[])?.len(), want.len(), "the root is everything");
    Ok(())
}
