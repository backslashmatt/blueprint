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
    assert_eq!(missing_lints(&g), Vec::<String>::new());
}

#[test]
fn no_source_outside_tests_switches_off_a_protected_lint() {
    let mut found = Vec::new();
    let mut stack = vec![root()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() && !name.starts_with('.') && name != "target" {
                stack.push(path);
            } else if name.ends_with(".rs") {
                let rel = path.strip_prefix(root()).unwrap().to_path_buf();
                let allowed = unsafe_allowed(&rel, |readme| {
                    std::fs::read_to_string(root().join(readme)).ok()
                });
                found.extend(lint_overrides(
                    &rel,
                    &std::fs::read_to_string(&path).unwrap(),
                    allowed,
                ));
            }
        }
    }
    assert_eq!(found, Vec::<String>::new());
}
