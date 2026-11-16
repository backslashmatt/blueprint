//! The crate layering rules of REQUIREMENTS §5.3, checked against the
//! dependency graph reported by `cargo metadata`.
//!
//! Dependencies count if they are normal or build dependencies, with all
//! features enabled; dev-dependencies do not count because they never reach a
//! shipped binary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Crates that make a crate a UI crate. `engine` must reach none of them.
pub const UI_TOOLKITS: &[&str] = &[
    "gpui",
    "slint",
    "wgpu",
    "winit",
    "egui",
    "eframe",
    "iced",
    "tao",
    "vello",
    "femtovg",
    "skia-safe",
    "glutin",
    "softbuffer",
    "raw-window-handle",
    "xilem",
    "druid",
    "makepad-widgets",
];

/// External crates `model` may use. §5.3: model has no I/O and no dependencies
/// on other crates; `serde` is allowed so model types can be serialised by
/// `protocol` (see PR #16).
pub const MODEL_ALLOWED_EXTERNAL: &[&str] = &["serde"];

/// One package in the graph.
#[derive(Debug, Clone, Default)]
pub struct Package {
    /// Path to its `Cargo.toml`.
    pub manifest_path: PathBuf,
    /// Whether it is a member of this workspace.
    pub workspace: bool,
    /// Names of its normal and build dependencies.
    pub deps: BTreeSet<String>,
}

/// The resolved dependency graph, keyed by package name.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    /// Every package in the resolve graph.
    pub packages: BTreeMap<String, Package>,
}

/// Why the graph could not be loaded.
#[derive(Debug)]
pub struct LoadError(pub String);

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LoadError {}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a Value, LoadError> {
    v.get(key)
        .ok_or_else(|| LoadError(format!("cargo metadata: missing `{key}`")))
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, LoadError> {
    field(v, key)?
        .as_str()
        .ok_or_else(|| LoadError(format!("cargo metadata: `{key}` is not a string")))
}

fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>, LoadError> {
    field(v, key)?
        .as_array()
        .ok_or_else(|| LoadError(format!("cargo metadata: `{key}` is not an array")))
}

impl Graph {
    /// Runs `cargo metadata` on the workspace at `root` and builds the graph.
    pub fn load(root: &Path) -> Result<Graph, LoadError> {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = Command::new(cargo)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--all-features",
                "--locked",
            ])
            .current_dir(root)
            .output()
            .map_err(|e| LoadError(format!("running cargo metadata: {e}")))?;
        if !out.status.success() {
            return Err(LoadError(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let meta: Value =
            serde_json::from_slice(&out.stdout).map_err(|e| LoadError(e.to_string()))?;
        Graph::from_metadata(&meta)
    }

    /// Builds the graph from parsed `cargo metadata` output.
    pub fn from_metadata(meta: &Value) -> Result<Graph, LoadError> {
        let mut by_id = BTreeMap::new();
        for p in array(meta, "packages")? {
            let pkg = Package {
                manifest_path: PathBuf::from(text(p, "manifest_path")?),
                ..Package::default()
            };
            by_id.insert(
                text(p, "id")?.to_owned(),
                (text(p, "name")?.to_owned(), pkg),
            );
        }
        for id in array(meta, "workspace_members")? {
            let id = id
                .as_str()
                .ok_or_else(|| LoadError("workspace member is not a string".into()))?;
            if let Some((_, pkg)) = by_id.get_mut(id) {
                pkg.workspace = true;
            }
        }
        let names: BTreeMap<String, String> = by_id
            .iter()
            .map(|(id, (name, _))| (id.clone(), name.clone()))
            .collect();
        for node in array(field(meta, "resolve")?, "nodes")? {
            let mut deps = BTreeSet::new();
            for d in array(node, "deps")? {
                let counts = array(d, "dep_kinds")?.iter().any(|k| {
                    let kind = k.get("kind").and_then(Value::as_str);
                    kind.is_none() || kind == Some("build")
                });
                if counts && let Some(name) = names.get(text(d, "pkg")?) {
                    deps.insert(name.clone());
                }
            }
            if let Some((_, pkg)) = by_id.get_mut(text(node, "id")?) {
                pkg.deps.extend(deps);
            }
        }
        Ok(Graph {
            packages: by_id.into_values().collect(),
        })
    }

    fn deps(&self, krate: &str) -> BTreeSet<String> {
        self.packages
            .get(krate)
            .map(|p| p.deps.clone())
            .unwrap_or_default()
    }

    fn is_workspace(&self, krate: &str) -> bool {
        self.packages.get(krate).is_some_and(|p| p.workspace)
    }

    fn workspace_deps(&self, krate: &str) -> BTreeSet<String> {
        self.deps(krate)
            .into_iter()
            .filter(|d| self.is_workspace(d))
            .collect()
    }

    fn reach(&self, krate: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![krate.to_owned()];
        while let Some(k) = stack.pop() {
            for d in self.deps(&k) {
                if seen.insert(d.clone()) {
                    stack.push(d);
                }
            }
        }
        seen
    }

    /// Language analyser crates: by name, or by living in `crates/lang-*`.
    fn language_crates(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|(name, p)| {
                let dir = p
                    .manifest_path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|d| d.to_str());
                p.workspace
                    && (name.starts_with("blueprint-lang-")
                        || dir.is_some_and(|d| d.starts_with("lang-")))
            })
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Every §5.3 layering violation in `g`, as a human-readable line.
pub fn violations(g: &Graph) -> Vec<String> {
    let mut v = Vec::new();
    let langs = g.language_crates();

    for d in g.deps("blueprint-model") {
        if g.is_workspace(&d) || !MODEL_ALLOWED_EXTERNAL.contains(&d.as_str()) {
            v.push(format!("blueprint-model may not depend on {d}"));
        }
    }
    for l in &langs {
        for d in g.workspace_deps(l) {
            if d != "blueprint-model" {
                v.push(format!("{l} may depend only on blueprint-model, not {d}"));
            }
        }
    }
    let front_ends = [
        "blueprint-engine",
        "blueprint-ui",
        "blueprint-app",
        "blueprint-cli",
    ];
    let protocol = g.reach("blueprint-protocol");
    for f in front_ends
        .iter()
        .map(|s| s.to_string())
        .chain(langs.iter().cloned())
    {
        if protocol.contains(&f) {
            v.push(format!("blueprint-protocol reaches {f}"));
        }
    }
    for d in g.workspace_deps("blueprint-ui") {
        if d != "blueprint-protocol" {
            v.push(format!(
                "blueprint-ui may depend on blueprint-protocol only, not {d}"
            ));
        }
    }
    let ui = g.reach("blueprint-ui");
    for f in std::iter::once("blueprint-engine".to_owned()).chain(langs.iter().cloned()) {
        if ui.contains(&f) {
            v.push(format!("blueprint-ui reaches {f}"));
        }
    }
    let engine = g.reach("blueprint-engine");
    for f in ["blueprint-ui", "blueprint-app"].iter().chain(UI_TOOLKITS) {
        if engine.contains(*f) {
            v.push(format!("blueprint-engine reaches UI crate {f}"));
        }
    }
    if g.is_workspace("blueprint-cli") && !g.deps("blueprint-cli").contains("blueprint-engine") {
        v.push("blueprint-cli must depend on blueprint-engine (everything the app shows, the CLI can print)".into());
    }
    v
}

/// Workspace members whose manifest does not inherit the workspace lints.
pub fn missing_lints(g: &Graph) -> Vec<String> {
    let mut v = Vec::new();
    for (name, p) in g.packages.iter().filter(|(_, p)| p.workspace) {
        let inherits = std::fs::read_to_string(&p.manifest_path).is_ok_and(|m| {
            let lines: Vec<&str> = m.lines().map(str::trim).collect();
            lines
                .windows(2)
                .any(|w| w == ["[lints]", "workspace = true"])
        });
        if !inherits {
            v.push(format!(
                "{name} ({}) does not inherit workspace lints",
                p.manifest_path.display()
            ));
        }
    }
    v
}

/// Lints that only test code may switch off (REQUIREMENTS §5.3).
pub const PROTECTED_LINTS: &[&str] = &[
    "unsafe_code",
    "unwrap_used",
    "expect_used",
    "panic",
    "todo",
    "unimplemented",
    "dbg_macro",
];

/// Crate directories, relative to the workspace root, that may allow
/// `unsafe_code`. Each must also justify it in its README under
/// [`UNSAFE_HEADING`] (REQUIREMENTS §5.3). Adding a crate here is a reviewed
/// change to this file.
pub const UNSAFE_CRATES: &[&str] = &["crates/store"];

/// The README heading that justifies a crate's `unsafe`.
pub const UNSAFE_HEADING: &str = "## Why this crate uses `unsafe`";

/// Whether source at `path` (relative to the workspace root, whose README
/// text is given by `readme`) may allow `unsafe_code`.
pub fn unsafe_allowed(path: &Path, readme: impl Fn(&Path) -> Option<String>) -> bool {
    UNSAFE_CRATES.iter().any(|c| {
        let dir = Path::new(c);
        path.starts_with(dir)
            && readme(&dir.join("README.md"))
                .is_some_and(|r| r.lines().any(|l| l.trim_end() == UNSAFE_HEADING))
    })
}

/// Source lines outside `tests/` directories that switch off a protected lint
/// with `allow`, `expect` or `warn`, as `path:line: text`. With
/// `unsafe_allowed`, switching off `unsafe_code` (and only that) is permitted.
pub fn lint_overrides(path: &Path, source: &str, unsafe_allowed: bool) -> Vec<String> {
    let in_tests_dir = path.components().any(|c| c.as_os_str() == "tests");
    if in_tests_dir {
        return Vec::new();
    }
    source
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let l = l.trim_start();
            l.starts_with("#")
                && ["allow(", "expect(", "warn("].iter().any(|a| l.contains(a))
                && PROTECTED_LINTS
                    .iter()
                    .filter(|lint| !(unsafe_allowed && **lint == "unsafe_code"))
                    .any(|lint| l.contains(lint))
        })
        .map(|(i, l)| format!("{}:{}: {}", path.display(), i + 1, l.trim()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(&str, &[&str])]) -> Graph {
        let mut g = Graph::default();
        for (name, deps) in edges {
            let p = Package {
                manifest_path: PathBuf::from(format!(
                    "/w/crates/{}/Cargo.toml",
                    name.trim_start_matches("blueprint-")
                )),
                workspace: name.starts_with("blueprint-") || name.starts_with("lang-"),
                deps: deps.iter().map(|d| d.to_string()).collect(),
            };
            g.packages.insert(name.to_string(), p);
        }
        g
    }

    fn good() -> Vec<(&'static str, &'static [&'static str])> {
        vec![
            ("blueprint-model", &["serde"]),
            ("serde", &[]),
            ("blueprint-protocol", &["blueprint-model"]),
            (
                "blueprint-engine",
                &[
                    "blueprint-model",
                    "blueprint-protocol",
                    "blueprint-lang-rust",
                ],
            ),
            ("blueprint-lang-rust", &["blueprint-model"]),
            ("blueprint-cli", &["blueprint-engine", "blueprint-protocol"]),
            ("blueprint-ui", &["blueprint-protocol"]),
            (
                "blueprint-app",
                &["blueprint-ui", "blueprint-engine", "gpui"],
            ),
            ("gpui", &[]),
            ("helper", &["gpui"]),
            ("tokio", &[]),
        ]
    }

    fn with(edit: (&'static str, &'static [&'static str])) -> Graph {
        let mut edges = good();
        edges.retain(|(n, _)| *n != edit.0);
        edges.push(edit);
        graph(&edges)
    }

    #[test]
    fn the_intended_layering_passes() {
        assert_eq!(violations(&graph(&good())), Vec::<String>::new());
    }

    #[test]
    fn each_rule_fires() {
        let cases: Vec<((&str, &[&str]), &str)> = vec![
            (
                ("blueprint-model", &["tokio"]),
                "blueprint-model may not depend on tokio",
            ),
            (
                ("blueprint-model", &["blueprint-protocol"]),
                "blueprint-model may not depend on blueprint-protocol",
            ),
            (
                (
                    "blueprint-lang-rust",
                    &["blueprint-model", "blueprint-engine"],
                ),
                "blueprint-lang-rust may depend only on",
            ),
            (
                ("lang-go", &["blueprint-protocol"]),
                "lang-go may depend only on",
            ),
            (
                (
                    "blueprint-protocol",
                    &["blueprint-model", "blueprint-engine"],
                ),
                "blueprint-protocol reaches blueprint-engine",
            ),
            (
                ("blueprint-ui", &["blueprint-protocol", "blueprint-model"]),
                "blueprint-ui may depend on blueprint-protocol only",
            ),
            (
                ("blueprint-ui", &["blueprint-protocol", "blueprint-cli"]),
                "blueprint-ui reaches blueprint-engine",
            ),
            (
                ("blueprint-engine", &["blueprint-model", "helper"]),
                "blueprint-engine reaches UI crate gpui",
            ),
            (
                ("blueprint-cli", &["blueprint-protocol"]),
                "blueprint-cli must depend on blueprint-engine",
            ),
        ];
        for (edit, expected) in cases {
            let v = violations(&with(edit));
            assert!(
                v.iter().any(|m| m.contains(expected)),
                "{edit:?}: expected `{expected}`, got {v:?}"
            );
        }
    }

    #[test]
    fn metadata_counts_build_deps_and_ignores_dev_deps() {
        let meta = serde_json::json!({
            "packages": [
                {"id": "e", "name": "blueprint-engine", "manifest_path": "/w/crates/engine/Cargo.toml"},
                {"id": "u", "name": "blueprint-ui", "manifest_path": "/w/crates/ui/Cargo.toml"},
                {"id": "t", "name": "gpui", "manifest_path": "/r/gpui/Cargo.toml"}
            ],
            "workspace_members": ["e", "u"],
            "resolve": {"nodes": [
                {"id": "e", "deps": [
                    {"pkg": "u", "dep_kinds": [{"kind": "build"}]},
                    {"pkg": "t", "dep_kinds": [{"kind": "dev"}]}
                ]},
                {"id": "u", "deps": []},
                {"id": "t", "deps": []}
            ]}
        });
        let g = Graph::from_metadata(&meta).unwrap();
        let v = violations(&g);
        assert!(
            v.iter()
                .any(|m| m.contains("reaches UI crate blueprint-ui")),
            "{v:?}"
        );
        assert!(!v.iter().any(|m| m.contains("gpui")), "{v:?}");
    }

    #[test]
    fn lint_overrides_are_found_outside_tests_only() {
        let src = "#![allow(clippy::unwrap_used)]\nfn f() {}\n#[allow(dead_code)]\n#[expect(unsafe_code)]\n";
        let found = lint_overrides(Path::new("crates/engine/src/lib.rs"), src, false);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(lint_overrides(Path::new("crates/engine/tests/it.rs"), src, false).is_empty());
    }

    #[test]
    fn unsafe_allowance_covers_unsafe_code_only() {
        let src = "#[allow(unsafe_code)]\n#[allow(unsafe_code, clippy::unwrap_used)]\n";
        let found = lint_overrides(Path::new("crates/store/src/lib.rs"), src, true);
        assert_eq!(found.len(), 1, "unwrap_used is still caught: {found:?}");
        assert_eq!(
            lint_overrides(Path::new("crates/store/src/lib.rs"), src, false).len(),
            2
        );
    }

    #[test]
    fn unsafe_needs_both_the_list_and_the_readme_heading() {
        let justified = |_: &Path| Some(format!("# store\n\n{UNSAFE_HEADING}\n\nBecause.\n"));
        let silent = |_: &Path| Some("# store\n\nNo reason given.\n".to_owned());
        let missing = |_: &Path| None;
        assert!(unsafe_allowed(
            Path::new("crates/store/src/segment.rs"),
            justified
        ));
        assert!(!unsafe_allowed(
            Path::new("crates/store/src/segment.rs"),
            silent
        ));
        assert!(!unsafe_allowed(
            Path::new("crates/store/src/segment.rs"),
            missing
        ));
        assert!(!unsafe_allowed(
            Path::new("crates/engine/src/lib.rs"),
            justified
        ));
        assert!(!unsafe_allowed(
            Path::new("crates/storefront/src/lib.rs"),
            justified
        ));
    }
}
