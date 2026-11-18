//! The engine service driven headless, as the app drives it (#89): open,
//! refs, a diff answered coarse then full, superseding, and source lines.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use blueprint_engine::service::Service;
use blueprint_model::diff::Finding;
use blueprint_protocol::{Detail, RefKind, Reply, Request, RequestId, Response};

type Files<'a> = &'a [(&'a str, &'a str)];

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

/// A repository with one commit per entry of `commits`, on `main`.
fn repo(name: &str, commits: &[Files<'_>]) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("service-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = gix::init(&dir).unwrap();
    std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let sig = gix::actor::SignatureRef {
        name: "Fixture".into(),
        email: "fixture@example.invalid".into(),
        time: "1700000000 +0000",
    };
    let mut parent: Option<gix::ObjectId> = None;
    for (n, files) in commits.iter().enumerate() {
        let blobs: Vec<(&str, gix::ObjectId)> = files
            .iter()
            .map(|(p, body)| (*p, repo.write_blob(body.as_bytes()).unwrap().detach()))
            .collect();
        let tree = write_tree(&repo, &blobs);
        let id = repo
            .commit_as(sig, sig, "HEAD", format!("commit {n}"), tree, parent)
            .unwrap()
            .detach();
        parent = Some(id);
    }
    dir
}

const WORKSPACE: &str = "[workspace]\nmembers = [\"a\", \"b\"]\n";
const BASE: Files<'_> = &[
    ("Cargo.toml", WORKSPACE),
    ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
    ("a/src/lib.rs", "pub fn f() -> u32 {\n    1\n}\n"),
    ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
    ("b/src/lib.rs", "pub fn g() {}\n"),
];
/// `b` now depends on `a`, and `f`'s body changed.
const HEAD: Files<'_> = &[
    ("Cargo.toml", WORKSPACE),
    ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
    ("a/src/lib.rs", "pub fn f() -> u32 {\n    2\n}\n"),
    (
        "b/Cargo.toml",
        "[package]\nname = \"b\"\n\n[dependencies]\na = { path = \"../a\" }\n",
    ),
    ("b/src/lib.rs", "pub fn g() {\n    a::f();\n}\n"),
];

fn start() -> (Service, Receiver<Reply>) {
    let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("service-cache-{}", std::process::id()));
    blueprint_engine::index::set_cache_root(cache);
    Service::start().unwrap()
}

/// Replies until `id` gets its last one, taking first any already received
/// while waiting for another request; replies to other requests are kept in
/// `others`.
fn until_last(rx: &Receiver<Reply>, id: RequestId, others: &mut Vec<Reply>) -> Vec<Response> {
    let mut mine = Vec::new();
    let (earlier, rest): (Vec<Reply>, Vec<Reply>) =
        std::mem::take(others).into_iter().partition(|r| r.id == id);
    *others = rest;
    for r in earlier {
        let last = r.response.is_last();
        mine.push(r.response);
        if last {
            return mine;
        }
    }
    loop {
        let r = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the engine answers within a minute");
        if r.id != id {
            others.push(r);
            continue;
        }
        let last = r.response.is_last();
        mine.push(r.response);
        if last {
            return mine;
        }
    }
}

fn failed(r: &Response) -> Option<&str> {
    match r {
        Response::Failed { error } => Some(error),
        _ => None,
    }
}

#[test]
fn open_refs_diff_and_source() {
    let dir = repo("flow", &[BASE, HEAD]);
    let (service, rx) = start();
    let mut others = Vec::new();

    service.send(
        1,
        Request::Diff {
            base: "main~1".into(),
            head: "main".into(),
        },
    );
    let r = until_last(&rx, 1, &mut others);
    assert_eq!(
        failed(&r[0]),
        Some("no repository is open"),
        "requests before Open fail, never panic"
    );

    service.send(
        2,
        Request::Open {
            path: dir.display().to_string(),
        },
    );
    let Response::Opened { head, .. } = &until_last(&rx, 2, &mut others)[0] else {
        panic!("not opened");
    };
    let head = head.clone().unwrap();
    assert_eq!(head.summary, "commit 1");

    service.send(3, Request::Refs { limit: 10 });
    let Response::Refs { refs, commits } = &until_last(&rx, 3, &mut others)[0] else {
        panic!("no refs");
    };
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].name, "refs/heads/main");
    assert_eq!(refs[0].kind, RefKind::Branch);
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].id, head.id);

    service.send(
        4,
        Request::Diff {
            base: "main~1".into(),
            head: "main".into(),
        },
    );
    let r = until_last(&rx, 4, &mut others);
    let details: Vec<Detail> = r
        .iter()
        .map(|r| match r {
            Response::Diff { detail, .. } => *detail,
            other => panic!("not a diff: {other:?}"),
        })
        .collect();
    assert_eq!(details, vec![Detail::Coarse, Detail::Full], "coarse first");
    for resp in &r {
        let Response::Diff { diff, .. } = resp else {
            unreachable!()
        };
        assert!(
            diff.findings.iter().any(|f| matches!(
                f,
                Finding::NewDependency { edge } if edge.from.to_string().contains("b")
                    && edge.to.to_string().contains("a")
            )),
            "b -> a is a new dependency at every detail: {:?}",
            diff.findings
        );
    }
    let (Response::Diff { diff: coarse, .. }, Response::Diff { diff: full, .. }) = (&r[0], &r[1])
    else {
        unreachable!()
    };
    assert!(coarse.weights.is_empty(), "weights come with the full diff");
    assert!(!full.weights.is_empty());
    assert!(full.nodes.len() > coarse.nodes.len(), "L3–L4 nodes too");

    // The second time, both commits are read from the index.
    service.send(
        5,
        Request::Diff {
            base: "main~1".into(),
            head: "main".into(),
        },
    );
    let again = until_last(&rx, 5, &mut others);
    let Response::Diff {
        diff: full_again, ..
    } = &again[1]
    else {
        panic!("no full diff");
    };
    assert_eq!(**full_again, **full);

    service.send(
        6,
        Request::Source {
            rev: "main".into(),
            path: "b/src/lib.rs".into(),
            first: 2,
            last: 3,
        },
    );
    let Response::Source { text } = &until_last(&rx, 6, &mut others)[0] else {
        panic!("no source");
    };
    assert_eq!(text, "    a::f();\n}");
    assert!(others.is_empty(), "no stray replies: {others:?}");
}

#[test]
fn a_newer_request_supersedes_an_older_one_in_its_lane() {
    let dir = repo("supersede", &[BASE, HEAD]);
    let (service, rx) = start();
    let mut others = Vec::new();
    service.send(
        1,
        Request::Open {
            path: dir.display().to_string(),
        },
    );
    until_last(&rx, 1, &mut others);

    // Sent back to back: the first cannot finish its L1–L4 stage (parsing
    // every file into an empty cache) before the second arrives.
    let diff = || Request::Diff {
        base: "main~1".into(),
        head: "main".into(),
    };
    service.send(2, diff());
    service.send(3, diff());
    // A request in another lane is not affected.
    service.send(4, Request::Model { rev: "main".into() });
    let superseded = until_last(&rx, 2, &mut others);
    assert!(
        matches!(superseded.last(), Some(Response::Superseded)),
        "{superseded:?}"
    );
    assert!(
        superseded.iter().all(|r| !matches!(
            r,
            Response::Diff {
                detail: Detail::Full,
                ..
            }
        )),
        "a superseded diff never sends its full answer"
    );
    let newest = until_last(&rx, 3, &mut others);
    assert!(matches!(
        newest.last(),
        Some(Response::Diff {
            detail: Detail::Full,
            ..
        })
    ));
    let model = until_last(&rx, 4, &mut others);
    assert!(matches!(
        model.last(),
        Some(Response::Model {
            detail: Detail::Full,
            ..
        })
    ));

    // Opening a repository supersedes everything sent before it.
    service.send(
        5,
        Request::Model {
            rev: "main~1".into(),
        },
    );
    service.send(
        6,
        Request::Open {
            path: dir.display().to_string(),
        },
    );
    let model = until_last(&rx, 5, &mut others);
    assert!(
        matches!(model.last(), Some(Response::Superseded)),
        "{model:?}"
    );
    until_last(&rx, 6, &mut others);
}

#[test]
fn bad_input_fails_and_the_service_keeps_answering() {
    let dir = repo("bad", &[BASE]);
    let (service, rx) = start();
    let mut others = Vec::new();
    service.send(
        1,
        Request::Open {
            path: "/nonexistent/blueprint/repo".into(),
        },
    );
    assert!(failed(&until_last(&rx, 1, &mut others)[0]).is_some());
    service.send(
        2,
        Request::Open {
            path: dir.display().to_string(),
        },
    );
    until_last(&rx, 2, &mut others);
    service.send(
        3,
        Request::Model {
            rev: "no-such-rev".into(),
        },
    );
    assert!(failed(&until_last(&rx, 3, &mut others)[0]).is_some());
    service.send(
        4,
        Request::Source {
            rev: "main".into(),
            path: "../etc/passwd".into(),
            first: 1,
            last: 1,
        },
    );
    assert!(failed(&until_last(&rx, 4, &mut others)[0]).is_some());
    service.send(5, Request::Model { rev: "main".into() });
    let r = until_last(&rx, 5, &mut others);
    assert!(matches!(
        r.last(),
        Some(Response::Model {
            detail: Detail::Full,
            ..
        })
    ));
}

#[test]
fn a_request_sent_after_open_reads_the_newly_opened_repository() {
    let one = repo("one-commit", &[BASE]);
    let two = repo("two-commits", &[BASE, HEAD]);
    let (service, rx) = start();
    let mut others = Vec::new();
    service.send(
        1,
        Request::Open {
            path: one.display().to_string(),
        },
    );
    until_last(&rx, 1, &mut others);
    // Back to back: the Model lane may pick this up before the Open of
    // `two` has finished, and must still read `two`, where `main~1` exists.
    service.send(
        2,
        Request::Open {
            path: two.display().to_string(),
        },
    );
    service.send(
        3,
        Request::Model {
            rev: "main~1".into(),
        },
    );
    let r = until_last(&rx, 3, &mut others);
    assert!(
        matches!(
            r.last(),
            Some(Response::Model {
                detail: Detail::Full,
                ..
            })
        ),
        "{r:?}"
    );
    until_last(&rx, 2, &mut others);
}

#[test]
fn requests_sent_from_many_threads_each_get_one_final_reply() {
    let a = repo("threads-a", &[BASE]);
    let b = repo("threads-b", &[BASE, HEAD]);
    let (service, rx) = start();
    let service = std::sync::Arc::new(service);
    let mut senders = Vec::new();
    for t in 0..4u64 {
        let service = std::sync::Arc::clone(&service);
        let (a, b) = (a.display().to_string(), b.display().to_string());
        senders.push(std::thread::spawn(move || {
            for i in 0..10u64 {
                let id = t * 100 + i * 2;
                let path = if (t + i) % 2 == 0 { &a } else { &b };
                service.send(id, Request::Open { path: path.clone() });
                service.send(id + 1, Request::Model { rev: "main".into() });
            }
        }));
    }
    for s in senders {
        s.join().unwrap();
    }
    let mut finals = std::collections::BTreeMap::new();
    while finals.len() < 80 {
        let r = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        if r.response.is_last() {
            assert!(
                finals.insert(r.id, ()).is_none(),
                "{} got two final replies",
                r.id
            );
        }
    }
}
