//! Python edges over whole trees (#157, DR 0014): what resolves, and what
//! must not be invented (§2.3).

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::analyse_deep;
use blueprint_engine::resident::{Resident, Update};
use blueprint_model::RepoPath;

use common::{Files, files};

const PY: (&str, &str) = ("pyproject.toml", "[project]\nname = \"p\"\n");

fn edges(entries: &[(&str, &str)]) -> Vec<String> {
    let tree = files(entries);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let (m, _) = analyse_deep(&paths, &|p| tree.get(p).cloned());
    m.edges()
        .map(|e| {
            format!(
                "{} -> {} {:?} {:?}",
                e.key.from, e.key.to, e.key.kind, e.confidence
            )
        })
        .collect()
}

fn has(e: &[String], s: &str) -> bool {
    e.iter().any(|x| x.contains(s))
}

/// Applies `text` to `path`, updates `r`, and checks it against a fresh
/// build.
fn change(r: &mut Resident, tree: &mut Files, path: &str, text: &str) -> Update {
    let path = RepoPath::parse(path).unwrap();
    tree.insert(path.clone(), text.as_bytes().to_vec());
    let how = r.update(&path, &|p| tree.get(p).cloned());
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let (fresh, diags) = analyse_deep(&paths, &|p| tree.get(p).cloned());
    assert_eq!(*r.model(), fresh, "after changing {path} ({how:?})");
    assert_eq!(r.diagnostics(), diags.as_slice());
    how
}

#[test]
fn relative_import_beyond_top_level_package() {
    let e = edges(&[
        PY,
        ("src/pkg/__init__.py", "from .. import other\n"),
        ("src/pkg/sub/__init__.py", ""),
        ("src/pkg/sub/mod.py", "from ... import other\n"),
        ("src/top.py", "from . import other\n"),
        ("src/other.py", "def f():\n    return 1\n"),
    ]);
    assert!(!has(&e, "-> L2:python:#other"), "{e:#?}");
}

#[test]
fn ambiguous_reading_overrides_a_real_binding() {
    let e = edges(&[
        PY,
        ("src/app/__init__.py", ""),
        ("src/app/db.py", "def init():\n    return 1\n"),
        ("src/other/__init__.py", ""),
        ("src/other/db.py", "def init():\n    return 2\n"),
        (
            "src/app/main.py",
            "from app import db\nimport other.db\n\ndef f():\n    return db.init()\n",
        ),
        ("src/app/logging.py", "def info():\n    return 1\n"),
        (
            "src/app/svc.py",
            "import logging\nimport app.logging\n\ndef g():\n    return logging.info()\n",
        ),
    ]);
    assert!(
        !has(&e, "app/main/(functions)/f -> L4:python:#other/db"),
        "{e:#?}"
    );
    assert!(
        !has(&e, "app/svc/(functions)/g -> L4:python:#app/logging"),
        "{e:#?}"
    );
}

#[test]
fn nested_class_base_is_the_class_body_name() {
    let e = edges(&[
        PY,
        (
            "src/app/m.py",
            "class A:\n    pass\n\nclass Outer:\n    class A:\n        def run(self):\n            return 1\n    class B(A):\n        def go(self):\n            return self.run()\n",
        ),
    ]);
    assert!(!has(&e, "Outer.B -> L3:python:#app/m/A Extends"), "{e:#?}");
}

#[test]
fn module_global_shadows_the_other_reading() {
    let e = edges(&[
        PY,
        (
            "src/app/__init__.py",
            "def create():\n    return 1\n\ndef run():\n    return 2\n",
        ),
        (
            "src/app/main.py",
            "from app import create\n\napp = create()\n\ndef main():\n    return app.run()\n",
        ),
    ]);
    assert!(
        !has(
            &e,
            "main/(functions)/main -> L4:python:#app/(functions)/run"
        ),
        "{e:#?}"
    );
}

#[test]
fn incremental_python() {
    let mut tree = files(&[
        PY,
        ("src/app/__init__.py", "def top():\n    return 1\n"),
        (
            "src/app/a.py",
            "class Base:\n    def run(self):\n        return 0\n",
        ),
        (
            "src/app/b.py",
            "from app.a import Base\nimport app\n\nclass K(Base):\n    def go(self):\n        return self.run() + self.extra() + app.top() + app.more()\n",
        ),
        (
            "src/app/c.py",
            "from app.b import K\nfrom app import more\n\ndef h():\n    return K() + more()\n",
        ),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    // A method a subclass in another file reaches.
    change(
        &mut r,
        &mut tree,
        "src/app/a.py",
        "class Base:\n    def run(self):\n        return 0\n    def extra(self):\n        return 1\n",
    );
    // Base renamed, then back.
    change(
        &mut r,
        &mut tree,
        "src/app/a.py",
        "class Base2:\n    def run(self):\n        return 0\n    def extra(self):\n        return 1\n",
    );
    change(
        &mut r,
        &mut tree,
        "src/app/a.py",
        "class Base:\n    def run(self):\n        return 0\n    def extra(self):\n        return 1\n",
    );
    // __init__ edits.
    change(
        &mut r,
        &mut tree,
        "src/app/__init__.py",
        "def top():\n    return 1\n\ndef more():\n    return 2\n",
    );
    change(
        &mut r,
        &mut tree,
        "src/app/__init__.py",
        "from .a import Base\n\ndef more():\n    return 2\n",
    );
    // Constructor appears.
    change(
        &mut r,
        &mut tree,
        "src/app/b.py",
        "from app.a import Base\nimport app\n\nclass K(Base):\n    def __init__(self):\n        pass\n    def go(self):\n        return self.run() + self.extra() + app.top() + app.more()\n",
    );
    // Import added and removed.
    change(
        &mut r,
        &mut tree,
        "src/app/c.py",
        "from app.b import K\nfrom app import more\nimport app.a\n\ndef h():\n    return K() + more() + app.a.Base()\n",
    );
    change(
        &mut r,
        &mut tree,
        "src/app/c.py",
        "def h():\n    return 1\n",
    );
}

#[test]
fn incremental_root_init() {
    let mut tree = files(&[
        PY,
        ("__init__.py", "def root():\n    return 1\n"),
        ("x.py", "from . import y\n\ndef f():\n    return 1\n"),
        ("y.py", "def g():\n    return 1\n"),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    change(
        &mut r,
        &mut tree,
        "x.py",
        "from . import root\n\ndef f():\n    return root()\n",
    );
    change(
        &mut r,
        &mut tree,
        "__init__.py",
        "def other():\n    return 1\n",
    );
    change(&mut r, &mut tree, "__init__.py", "");
    change(&mut r, &mut tree, "x.py", "def f():\n    return 1\n");
    change(&mut r, &mut tree, "__init__.py", "from .x import f\n");
}

#[test]
fn self_call_through_a_base_skips_the_mro() {
    let e = edges(&[
        PY,
        (
            "src/app/a.py",
            "class Root:\n    def m(self):\n        return 0\n\nclass B1(Root):\n    pass\n\nclass B2:\n    def m(self):\n        return 1\n",
        ),
        (
            "src/app/b.py",
            "from app.a import B1, B2\nfrom django.views import LoginRequiredMixin\n\nclass K(B1, B2):\n    def go(self):\n        return self.m()\n\nclass V(LoginRequiredMixin, B2):\n    def go(self):\n        return self.m()\n",
        ),
    ]);
    assert!(!has(&e, "app/b/K/go -> L4:python:#app/a/B2/m"), "{e:#?}");
    assert!(!has(&e, "app/b/V/go -> L4:python:#app/a/B2/m"), "{e:#?}");
}

#[test]
fn missed_edges() {
    let e = edges(&[
        PY,
        (
            "src/app/a.py",
            "class Config:\n    def load(self):\n        return 0\n",
        ),
        (
            "src/app/b.py",
            "from app.a import Config\n\nclass Config(Config):\n    pass\n\nclass Outer:\n    class Inner:\n        def __init__(self):\n            pass\n\ndef f():\n    return Outer.Inner()\n",
        ),
    ]);
    // `class Config(Config)` extends the imported one, not itself.
    assert!(
        has(
            &e,
            "L3:python:#app/b/Config -> L3:python:#app/a/Config Extends"
        ),
        "{e:#?}"
    );
    // `Outer.Inner()` calls the nested class's constructor.
    assert!(
        has(
            &e,
            "(functions)/f -> L4:python:#app/b/Outer.Inner/__init__ Calls"
        ),
        "{e:#?}"
    );
}

#[test]
fn incremental_two_projects() {
    let mut tree = files(&[
        ("core/pyproject.toml", "[project]\nname = \"core\"\n"),
        ("core/src/core/x.py", "def f():\n    return 1\n"),
        (
            "web/pyproject.toml",
            "[project]\nname = \"web\"\ndependencies = [\"core\"]\n",
        ),
        (
            "web/web/app.py",
            "from core.x import f, g\nimport core\n\ndef run():\n    return f() + g() + core.h()\n",
        ),
    ]);
    let paths: Vec<RepoPath> = tree.keys().cloned().collect();
    let mut r = Resident::build(&paths, &|p| tree.get(p).cloned());
    change(
        &mut r,
        &mut tree,
        "core/src/core/x.py",
        "def f():\n    return 1\n\ndef g():\n    return 2\n",
    );
    change(
        &mut r,
        &mut tree,
        "core/src/core/__init__.py",
        "def h():\n    return 1\n",
    );
    change(
        &mut r,
        &mut tree,
        "core/src/core/__init__.py",
        "def h2():\n    return 1\n",
    );
    // web gets its own `core` package: resolves first.
    change(
        &mut r,
        &mut tree,
        "web/core/__init__.py",
        "def h():\n    return 1\n",
    );
    change(
        &mut r,
        &mut tree,
        "web/core/__init__.py",
        "def h3():\n    return 1\n",
    );
    change(
        &mut r,
        &mut tree,
        "web/core/x.py",
        "def g():\n    return 1\n",
    );
    change(
        &mut r,
        &mut tree,
        "web/core/x.py",
        "class g:\n    def __init__(self):\n        pass\n",
    );
}
