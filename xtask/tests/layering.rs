//! Runs the REQUIREMENTS §5.3 layering and lint checks on this workspace.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::path::{Path, PathBuf};

use xtask::layering::{Graph, lint_overrides, missing_lints, unsafe_allowed, violations};

/// The workspace root. Read at run time, not compile time, so a test binary
/// reused from a shared target directory still checks this checkout.
fn root() -> PathBuf {
    let dir =
        std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").into());
    Path::new(&dir).join("..")
}

#[test]
fn crate_layering_follows_section_5_3() {
    let g = Graph::load(&root()).expect("load cargo metadata");
    assert!(g.packages.values().filter(|p| p.workspace).count() >= 8);
    assert_eq!(violations(&g), Vec::<String>::new());
}

#[test]
fn every_workspace_crate_inherits_workspace_lints() {
    let g = Graph::load(&root()).expect("load cargo metadata");
    assert_eq!(missing_lints(&g, &root()), Vec::<String>::new());
}

/// Whether `path` is under a `tests/` directory directly inside a crate
/// (a directory with a `Cargo.toml`), which is where integration tests live.
fn is_test_file(path: &Path) -> bool {
    path.ancestors().skip(1).any(|d| {
        d.file_name().is_some_and(|n| n == "tests")
            && d.parent().is_some_and(|c| c.join("Cargo.toml").is_file())
    })
}

/// Every `.rs` file in the workspace, skipping hidden directories, build
/// output and the fetched reference repositories.
fn sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if !name.starts_with('.') && name != "target" && path != root().join("bench/repos")
                {
                    stack.push(path);
                }
            } else if name.ends_with(".rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn no_source_outside_tests_switches_off_a_protected_lint() {
    let mut found = Vec::new();
    for path in sources() {
        let rel = path.strip_prefix(root()).unwrap().to_path_buf();
        let allowed = unsafe_allowed(&rel, |readme| {
            std::fs::read_to_string(root().join(readme)).ok()
        });
        found.extend(lint_overrides(
            &rel,
            &std::fs::read_to_string(&path).unwrap(),
            allowed,
            is_test_file(&path),
        ));
    }
    assert_eq!(found, Vec::<String>::new());
}

/// The store's README promises exactly one `unsafe` block; hold it to that.
#[test]
fn the_store_has_exactly_one_unsafe_block() {
    let store = root().join("crates/store/src");
    let count: usize = sources()
        .iter()
        .filter(|p| p.starts_with(&store))
        .map(|p| {
            std::fs::read_to_string(p)
                .unwrap()
                .matches("unsafe ")
                .count()
        })
        .sum();
    assert_eq!(
        count, 1,
        "crates/store/README.md documents exactly one unsafe call"
    );
}
