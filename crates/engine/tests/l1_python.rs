//! L1 Python analysis over fixtures, as text snapshots in
//! `tests/snapshots/`. Set `UPDATE_SNAPSHOTS=1` to rewrite them.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{Diagnostic, python};
use blueprint_model::{Confidence, Model, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let mut model = Model::default();
    let diags = python::analyse(&paths, &|p| files.get(p).cloned(), &mut model);
    (model, diags)
}

fn check(name: &str, entries: &[(&str, &str)]) -> (Model, Vec<Diagnostic>) {
    let (model, diags) = run(&files(entries));
    snapshot(name, &render(&model, &diags));
    (model, diags)
}

fn edges(model: &Model) -> Vec<String> {
    model
        .edges()
        .map(|e| {
            format!(
                "{} -> {} {:?}",
                e.key.from.system_key().dir,
                e.key.to.system_key().dir,
                e.confidence
            )
        })
        .collect()
}

fn systems(model: &Model) -> Vec<(String, String, Confidence)> {
    model
        .nodes()
        .map(|n| {
            (
                n.id.system_key().dir.to_string(),
                n.name.clone(),
                n.confidence,
            )
        })
        .collect()
}

#[test]
fn uv_workspace_with_workspace_and_path_sources() {
    let (model, diags) = check(
        "l1_python_uv",
        &[
            (
                "pyproject.toml",
                "[tool.uv.workspace]\nmembers = [\"packages/*\"]\n[tool.ruff]\nline-length = 100\n",
            ),
            (
                "packages/app/pyproject.toml",
                r#"[project]
name = "app"
dependencies = ["core>=1", "Shared_Utils", "requests", "remote @ https://example.com/r.whl"]
optional-dependencies = { test = ["testkit"] }
[tool.uv.sources]
core = { workspace = true }
shared-utils = { path = "../../libs/shared" }
requests = { git = "https://github.com/psf/requests" }
"#,
            ),
            (
                "packages/core/pyproject.toml",
                "[project]\nname = \"core\"\ndependencies = [\"shared-utils\"]\n",
            ),
            (
                "libs/shared/pyproject.toml",
                "[build-system]\nrequires = [\"hatchling\"]\n[project]\nname = \"shared.utils\"\n",
            ),
            (
                "tests/testkit/setup.py",
                "from setuptools import setup\nsetup(name='testkit')\n",
            ),
            (
                ".venv/lib/site-packages/requests/pyproject.toml",
                "[project]\nname = \"requests\"\n",
            ),
        ],
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        systems(&model),
        vec![
            (
                "libs/shared".into(),
                "shared.utils".into(),
                Confidence::Exact
            ),
            ("packages/app".into(), "app".into(), Confidence::Exact),
            ("packages/core".into(), "core".into(), Confidence::Exact),
            (
                "tests/testkit".into(),
                "testkit".into(),
                Confidence::Inferred
            ),
        ]
    );
    assert_eq!(
        edges(&model),
        vec![
            "packages/app -> libs/shared Exact",
            "packages/app -> packages/core Exact",
            "packages/core -> libs/shared Inferred",
        ]
    );
}

#[test]
fn poetry_path_dependencies_and_setup_cfg_names() {
    let (model, diags) = check(
        "l1_python_poetry",
        &[
            (
                "svc/pyproject.toml",
                r#"[tool.poetry]
name = "svc"
[tool.poetry.dependencies]
python = "^3.11"
lib = { path = "../lib", develop = true }
legacy = "*"
gone = { path = "../nowhere" }
[tool.poetry.group.dev.dependencies]
tools = { path = "../tools" }
"#,
            ),
            ("lib/pyproject.toml", "[tool.poetry]\nname = \"lib\"\n"),
            (
                "legacy/setup.cfg",
                "[metadata]\nname = Legacy\nversion = 1\n",
            ),
            ("legacy/setup.py", "import setuptools; setuptools.setup()\n"),
            ("tools/setup.py", "setup()\n"),
        ],
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        systems(&model),
        vec![
            ("legacy".into(), "Legacy".into(), Confidence::Inferred),
            ("lib".into(), "lib".into(), Confidence::Exact),
            ("svc".into(), "svc".into(), Confidence::Exact),
            ("tools".into(), "tools".into(), Confidence::Inferred),
        ]
    );
    // Dev groups do not ship; `../nowhere` is no project.
    assert_eq!(
        edges(&model),
        vec!["svc -> legacy Inferred", "svc -> lib Exact"]
    );
}

#[test]
fn broken_and_ambiguous_manifests_degrade() {
    let (model, diags) = check(
        "l1_python_broken",
        &[
            ("a/pyproject.toml", "[project\nname = "),
            ("b/pyproject.toml", "[project]\nname = \"dup\"\n"),
            ("c/pyproject.toml", "[project]\nname = \"dup\"\n"),
            (
                "d/pyproject.toml",
                "[project]\nname = \"d\"\ndependencies = [\"dup\", \"x\"]\n[tool.uv.sources]\nx = { workspace = true }\ny = { path = \"../../..\" }\n",
            ),
            (
                "e/pyproject.toml",
                "[project]\nname = \"e\"\ndependencies = [\"y\"]\n[tool.uv.sources]\ny = { path = \"../../..\" }\n",
            ),
        ],
    );
    assert_eq!(model.edges().count(), 0, "{:?}", edges(&model));
    let a = model
        .nodes()
        .find(|n| n.id.system_key().dir.to_string() == "a")
        .unwrap();
    assert_eq!((a.name.as_str(), a.confidence), ("a", Confidence::Unknown));
    let messages: Vec<String> = diags
        .iter()
        .map(|d| format!("{}: {}", d.path, d.message.lines().next().unwrap_or("")))
        .collect();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert!(messages[0].starts_with("a/pyproject.toml"));
    assert!(messages[1].contains("`x` is a workspace source"));
    assert!(messages[2].contains("outside the repository"));
}
