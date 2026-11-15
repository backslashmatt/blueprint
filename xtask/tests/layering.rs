//! Enforces the crate layering rules of REQUIREMENTS §5.3 against the real
//! dependency graph reported by `cargo metadata`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Crates that would make a crate a UI crate. `engine` must reach none of these.
const UI_TOOLKITS: &[&str] = &["gpui", "slint", "wgpu", "winit", "egui", "iced", "tao"];

struct Graph {
    /// Package name -> names of its normal (non-dev, non-build) dependencies.
    deps: BTreeMap<String, BTreeSet<String>>,
    workspace: BTreeSet<String>,
    /// Manifest paths of every workspace member.
    manifests: Vec<String>,
}

impl Graph {
    fn load() -> Graph {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args(["metadata", "--format-version", "1"])
            .current_dir(&root)
            .output()
            .expect("run cargo metadata");
        assert!(
            out.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let meta: Value = serde_json::from_slice(&out.stdout).expect("parse metadata");

        let mut names = BTreeMap::new();
        let mut paths = BTreeMap::new();
        for p in meta["packages"].as_array().expect("packages") {
            paths.insert(
                p["id"].as_str().expect("id").to_owned(),
                p["manifest_path"]
                    .as_str()
                    .expect("manifest_path")
                    .to_owned(),
            );
            names.insert(
                p["id"].as_str().expect("id").to_owned(),
                p["name"].as_str().expect("name").to_owned(),
            );
        }
        let manifests = meta["workspace_members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|id| paths[id.as_str().expect("id")].clone())
            .collect();
        let workspace = meta["workspace_members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|id| names[id.as_str().expect("id")].clone())
            .collect();

        let mut deps = BTreeMap::new();
        for node in meta["resolve"]["nodes"].as_array().expect("nodes") {
            let name = names[node["id"].as_str().expect("id")].clone();
            let normal: BTreeSet<String> = node["deps"]
                .as_array()
                .expect("deps")
                .iter()
                .filter(|d| {
                    d["dep_kinds"]
                        .as_array()
                        .expect("kinds")
                        .iter()
                        .any(|k| k["kind"].is_null())
                })
                .map(|d| names[d["pkg"].as_str().expect("pkg")].clone())
                .collect();
            deps.entry(name)
                .or_insert_with(BTreeSet::new)
                .extend(normal);
        }
        Graph {
            deps,
            workspace,
            manifests,
        }
    }

    fn direct_workspace_deps(&self, krate: &str) -> BTreeSet<String> {
        self.deps[krate]
            .iter()
            .filter(|d| self.workspace.contains(*d))
            .cloned()
            .collect()
    }

    fn transitive(&self, krate: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![krate.to_owned()];
        while let Some(k) = stack.pop() {
            for d in self.deps.get(&k).into_iter().flatten() {
                if seen.insert(d.clone()) {
                    stack.push(d.clone());
                }
            }
        }
        seen
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn model_depends_on_no_workspace_crate() {
    let g = Graph::load();
    assert_eq!(g.direct_workspace_deps("blueprint-model"), set(&[]));
}

#[test]
fn language_crates_depend_only_on_model() {
    let g = Graph::load();
    for krate in g
        .workspace
        .iter()
        .filter(|k| k.starts_with("blueprint-lang-"))
    {
        let extra: BTreeSet<_> = g
            .direct_workspace_deps(krate)
            .difference(&set(&["blueprint-model"]))
            .cloned()
            .collect();
        assert!(
            extra.is_empty(),
            "{krate} may depend only on blueprint-model, also has {extra:?}"
        );
    }
}

#[test]
fn protocol_does_not_reach_engine_or_front_ends() {
    let g = Graph::load();
    let reach = g.transitive("blueprint-protocol");
    for forbidden in [
        "blueprint-engine",
        "blueprint-ui",
        "blueprint-app",
        "blueprint-cli",
    ] {
        assert!(
            !reach.contains(forbidden),
            "blueprint-protocol reaches {forbidden}"
        );
    }
}

#[test]
fn ui_talks_to_engine_only_through_protocol() {
    let g = Graph::load();
    let direct = g.direct_workspace_deps("blueprint-ui");
    assert!(
        direct.is_subset(&set(&["blueprint-protocol", "blueprint-model"])),
        "blueprint-ui has {direct:?}"
    );
    assert!(
        !g.transitive("blueprint-ui").contains("blueprint-engine"),
        "blueprint-ui reaches blueprint-engine"
    );
}

#[test]
fn engine_has_no_ui_dependency() {
    let g = Graph::load();
    let reach = g.transitive("blueprint-engine");
    for forbidden in ["blueprint-ui", "blueprint-app"].iter().chain(UI_TOOLKITS) {
        assert!(
            !reach.contains(*forbidden),
            "blueprint-engine reaches {forbidden}"
        );
    }
}

#[test]
fn every_crate_inherits_workspace_lints() {
    let g = Graph::load();
    assert!(
        g.manifests.len() >= 8,
        "expected every workspace crate, got {:?}",
        g.manifests
    );
    for manifest_path in &g.manifests {
        let manifest = std::fs::read_to_string(manifest_path).expect("read manifest");
        // Compare trimmed lines so the check does not depend on line endings.
        let lines: Vec<&str> = manifest.lines().map(str::trim).collect();
        let inherits = lines
            .windows(2)
            .any(|w| w == ["[lints]", "workspace = true"]);
        assert!(inherits, "{manifest_path} does not inherit workspace lints");
    }
}
