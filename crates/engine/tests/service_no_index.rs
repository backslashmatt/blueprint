//! The engine service when the index cannot be written: models and diffs are
//! still answered, analysed without it, as `bp model` and `bp diff` do.
//! A test binary of its own, as the cache directory is set once per process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

use std::path::PathBuf;
use std::time::Duration;

use blueprint_engine::service::Service;
use blueprint_protocol::{Detail, Request, Response};

#[test]
fn models_and_diffs_are_answered_without_a_usable_index() {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("service-no-index-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    // A cache directory under a regular file can never be created.
    let file = tmp.join("not-a-directory");
    std::fs::write(&file, "").unwrap();
    assert!(blueprint_engine::index::set_cache_root(file.join("cache")));

    let dir = tmp.join("repo");
    let repo = gix::init(&dir).unwrap();
    std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let sig = gix::actor::SignatureRef {
        name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        time: "1700000000 +0000",
    };
    let mut parent: Option<gix::ObjectId> = None;
    for body in ["pub fn f() {}\n", "pub fn f() {}\npub fn g() {}\n"] {
        use gix::objs::tree::{Entry, EntryKind};
        let lib = repo.write_blob(body.as_bytes()).unwrap().detach();
        let manifest = repo
            .write_blob(b"[package]\nname = \"a\"\n")
            .unwrap()
            .detach();
        let mut src = gix::objs::Tree::empty();
        src.entries.push(Entry {
            mode: EntryKind::Blob.into(),
            filename: "lib.rs".into(),
            oid: lib,
        });
        let src = repo.write_object(&src).unwrap().detach();
        let mut root = gix::objs::Tree::empty();
        root.entries.push(Entry {
            mode: EntryKind::Blob.into(),
            filename: "Cargo.toml".into(),
            oid: manifest,
        });
        root.entries.push(Entry {
            mode: EntryKind::Tree.into(),
            filename: "src".into(),
            oid: src,
        });
        root.entries.sort();
        let tree = repo.write_object(&root).unwrap().detach();
        parent = Some(
            repo.commit_as(sig, sig, "HEAD", "c", tree, parent)
                .unwrap()
                .detach(),
        );
    }

    let (service, rx) = Service::start().unwrap();
    service.send(
        1,
        Request::Open {
            path: dir.display().to_string(),
        },
    );
    service.send(2, Request::Model { rev: "main".into() });
    service.send(
        3,
        Request::Diff {
            base: "main~1".into(),
            head: "main".into(),
        },
    );
    let mut last = std::collections::HashMap::new();
    while last.len() < 3 {
        let r = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        if r.response.is_last() {
            last.insert(r.id, r.response);
        }
    }
    assert!(
        matches!(last[&1], Response::Opened { .. }),
        "{:?}",
        last[&1]
    );
    assert!(
        matches!(
            last[&2],
            Response::Model {
                detail: Detail::Full,
                ..
            }
        ),
        "{:?}",
        last[&2]
    );
    let Response::Diff {
        detail: Detail::Full,
        diff,
        ..
    } = &last[&3]
    else {
        panic!("{:?}", last[&3]);
    };
    assert!(!diff.nodes.is_empty(), "g was added");
}
