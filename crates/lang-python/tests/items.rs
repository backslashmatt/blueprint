//! The Python analyser's facts.

use blueprint_lang_python::{Python, analyse};
use blueprint_model::lang::{FileFacts, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};

fn py(src: &str) -> FileFacts {
    analyse(src.as_bytes())
}

fn by_name<'a>(f: &'a FileFacts, name: &str) -> &'a Item {
    let found = f.items.iter().find(|i| i.name == name);
    found.unwrap_or_else(|| unreachable!("no item {name} in {:#?}", f.items))
}

fn item(src: &str, name: &str) -> Item {
    by_name(&py(src), name).clone()
}

fn p(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| (*x).to_owned()).collect()
}

const SAMPLE: &str = r#""""Module docstring."""
import os, a.b as ab
from ..pkg.mod import Thing as T, other
from . import sibling
from typing import *
from __future__ import annotations

@dataclass
class Shape(Base, abc.ABC, Generic[T], metaclass=Meta):
    """A shape."""
    sides: int = 3

    def __init__(self, n):
        self.n = n
        self.setup()

    @staticmethod
    def make(x):
        return helper(x)

    async def _area(self) -> float:
        return math.pi

    class Inner:
        def go(cls):
            pass

def top(a, *rest, key=None, **kw):
    for i in rest:
        i.run()
    return build(a)

def _private():
    pass

if TYPE_CHECKING:
    def checked(): ...

type Alias = list[int]
"#;

#[test]
fn finds_classes_and_functions_in_source_order() {
    let f = py(SAMPLE);
    assert!(f.clean, "{f:#?}");
    let got: Vec<(&str, String, Option<Visibility>)> = f
        .items
        .iter()
        .map(|i| {
            let kind = match &i.kind {
                ItemKind::Type { keyword } => keyword.clone(),
                ItemKind::Method { self_type, .. } => format!("method of {self_type}"),
                ItemKind::Function => "function".into(),
            };
            (i.name.as_str(), kind, i.visibility)
        })
        .collect();
    use Visibility::{Private, Public};
    assert_eq!(
        got,
        vec![
            ("Shape", "class".into(), Some(Public)),
            ("__init__", "method of Shape".into(), Some(Public)),
            ("make", "method of Shape".into(), Some(Public)),
            ("_area", "method of Shape".into(), Some(Private)),
            ("Shape.Inner", "class".into(), Some(Public)),
            ("go", "method of Shape.Inner".into(), Some(Public)),
            ("top", "function".into(), Some(Public)),
            ("_private", "function".into(), Some(Private)),
            ("checked", "function".into(), Some(Public)),
            ("Alias", "type".into(), Some(Public)),
        ]
    );
    assert!(f.items.iter().all(|i| i.module.is_empty()));
    // The class starts at its decorator.
    assert_eq!(by_name(&f, "Shape").lines.0, 8);
    assert_eq!(by_name(&f, "Alias").body_hash, None);
}

#[test]
fn imports_and_bases_are_references() {
    let f = py(SAMPLE);
    let refs: Vec<String> = f
        .references
        .iter()
        .map(|r| match r {
            Reference::Use { path, alias, .. } => format!("use {} {alias:?}", path.join("/")),
            Reference::Impl {
                self_type,
                r#trait,
                extends,
                ..
            } => format!("{} : {} {extends}", self_type.join("."), r#trait.join(".")),
        })
        .collect();
    assert_eq!(
        refs,
        [
            "use os None",
            "use a/b Some(\"ab\")",
            "use ../pkg/mod/Thing Some(\"T\")",
            "use ../pkg/mod/other None",
            "use ./sibling None",
            "use typing/* None",
            "Shape : Base true",
            "Shape : abc.ABC true",
            "Shape : Generic true",
        ]
    );
}

#[test]
fn imports_inside_functions_and_blocks_count() {
    let f = py(
        "def f():\n    import json\n    return json.dumps(1)\ntry:\n    import ujson as json\nexcept ImportError:\n    pass\n",
    );
    let paths: Vec<Vec<String>> = f
        .references
        .iter()
        .filter_map(|r| match r {
            Reference::Use { path, .. } => Some(path.clone()),
            Reference::Impl { .. } => None,
        })
        .collect();
    assert_eq!(paths, vec![p(&["json"]), p(&["ujson"])]);
    // A module imported locally is still called through.
    assert_eq!(by_name(&f, "f").calls, vec![p(&["json", "dumps"])]);
}

#[test]
fn calls_skip_locals_and_name_the_receiver_self() {
    let f = py(SAMPLE);
    assert_eq!(by_name(&f, "__init__").calls, vec![p(&["self", "setup"])]);
    // Static: `x` is an ordinary parameter.
    assert_eq!(by_name(&f, "make").calls, vec![p(&["helper"])]);
    // `i` is a loop variable.
    assert_eq!(by_name(&f, "top").calls, vec![p(&["build"])]);
    assert!(by_name(&f, "Shape").calls.is_empty());
    let m = item(
        "class C:\n    def m(this, xs):\n        this.a.b()\n        this()\n        y = g()\n        y.h()\n        with open(p) as fh:\n            fh.read()\n        [z.q() for z in xs]\n        (lambda t: t.u())(1)\n        if (w := k()):\n            w.v()\n        def inner():\n            pass\n        inner()\n        super().x()\n        try:\n            pass\n        except E as e:\n            e.log()\n",
        "m",
    );
    assert_eq!(
        m.calls,
        vec![
            p(&["self", "a", "b"]),
            p(&["g"]),
            p(&["open"]),
            p(&["k"]),
            p(&["super"]),
        ]
    );
    // `global` names refer outwards.
    let g = item(
        "def f():\n    global log\n    log = make()\n    log.write()\n",
        "f",
    );
    assert_eq!(g.calls, vec![p(&["make"]), p(&["log", "write"])]);
}

#[test]
fn body_changes_keep_the_signature_and_signature_changes_show() {
    let base = "def f(a: int) -> int:\n    \"\"\"Doc.\"\"\"\n    return a\n";
    let f0 = item(base, "f");
    // Comments, blank lines and re-indenting change nothing.
    let same = item(
        "def f(a: int) -> int:  # c\n\n        \"\"\"Doc.\"\"\"\n        return a  # c\n",
        "f",
    );
    assert_eq!(f0.signature_hash, same.signature_hash);
    assert_eq!(f0.body_hash, same.body_hash);
    // The docstring is body.
    let doc = item(
        "def f(a: int) -> int:\n    \"\"\"Other.\"\"\"\n    return a\n",
        "f",
    );
    assert_eq!(f0.signature_hash, doc.signature_hash);
    assert_ne!(f0.body_hash, doc.body_hash);
    for changed in [
        "def f(a: str) -> int:\n    \"\"\"Doc.\"\"\"\n    return a\n",
        "def f(a: int) -> str:\n    \"\"\"Doc.\"\"\"\n    return a\n",
        "async def f(a: int) -> int:\n    \"\"\"Doc.\"\"\"\n    return a\n",
        "@cache\ndef f(a: int) -> int:\n    \"\"\"Doc.\"\"\"\n    return a\n",
        "def f(a: int = 1) -> int:\n    \"\"\"Doc.\"\"\"\n    return a\n",
    ] {
        let c = item(changed, "f");
        assert_ne!(f0.signature_hash, c.signature_hash, "{changed}");
        assert_eq!(f0.body_hash, c.body_hash, "{changed}");
    }
}

#[test]
fn moving_a_statement_out_of_a_block_changes_the_body() {
    let inside = item("def f(a):\n    if a:\n        x()\n        y()\n", "f");
    let outside = item("def f(a):\n    if a:\n        x()\n    y()\n", "f");
    assert_ne!(inside.body_hash, outside.body_hash);
}

#[test]
fn class_attributes_are_signature_and_their_values_body() {
    let base = item("class C(B):\n    x: int = 1\n    def m(self): pass\n", "C");
    let value = item("class C(B):\n    x: int = 2\n    def m(self): pass\n", "C");
    assert_eq!(base.signature_hash, value.signature_hash);
    assert_ne!(base.body_hash, value.body_hash);
    let method = item(
        "class C(B):\n    x: int = 1\n    def m(self): return 1\n",
        "C",
    );
    assert_eq!(base.signature_hash, method.signature_hash);
    assert_ne!(base.body_hash, method.body_hash);
    for changed in [
        "class C(D):\n    x: int = 1\n    def m(self): pass\n",
        "class C(B):\n    x: str = 1\n    def m(self): pass\n",
        "class C(B):\n    y: int = 1\n    def m(self): pass\n",
        "@final\nclass C(B):\n    x: int = 1\n    def m(self): pass\n",
    ] {
        assert_ne!(
            base.signature_hash,
            item(changed, "C").signature_hash,
            "{changed}"
        );
    }
    // A class of only `pass` has no body.
    assert_eq!(item("class E(Exception):\n    pass\n", "E").body_hash, None);
}

#[test]
fn definitions_inside_functions_are_not_items() {
    let f = py(
        "def outer():\n    class Local:\n        def m(self): pass\n    def helper(): pass\n    return helper()\n",
    );
    let names: Vec<&str> = f.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["outer"]);
}

#[test]
fn handles_py_and_pyi_files_only() {
    let path = |s: &str| RepoPath::parse(s).unwrap_or_else(|_| unreachable!());
    assert!(Python.handles(&path("pkg/mod.py")));
    assert!(Python.handles(&path("pkg/mod.pyi")));
    assert!(!Python.handles(&path("pkg/mod.pyc")));
    assert!(!Python.handles(&path("setup.cfg")));
}

#[test]
fn broken_and_hostile_input_never_panics() {
    let deep = "class A:\n".to_owned()
        + &(1..200)
            .map(|i| format!("{}class A{i}:\n", "    ".repeat(i)))
            .collect::<String>()
        + &"    ".repeat(200)
        + "pass\n";
    let nested_parens = "x = ".to_owned() + &"(".repeat(5000) + &")".repeat(5000);
    for src in [
        "def f(:\n    pass\n",
        "class C(\n",
        "\u{feff}def f(): pass",
        "\u{0}\u{1}",
        &deep,
        &nested_parens,
    ] {
        let f = py(src);
        let _ = Python.highlight(src.as_bytes());
        assert!(f.items.len() < 200, "{}", f.items.len());
    }
    // An unclosed class still yields what was readable.
    let f = py("class C:\n    def m(self):\n        return 1\n    def n(self\n");
    assert!(!f.clean);
    assert!(f.items.iter().any(|i| i.name == "m"), "{:#?}", f.items);
}
