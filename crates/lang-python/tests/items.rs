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

// Regressions from review (#153).

fn names(f: &FileFacts) -> Vec<&str> {
    f.items.iter().map(|i| i.name.as_str()).collect()
}

#[test]
fn definitions_under_every_clause_are_items() {
    let f = py(
        "try:\n import x\nexcept ImportError:\n def f(): pass\nelse:\n def g(): pass\nfinally:\n def h(): pass\nif a:\n pass\nelif b:\n def i(): pass\nelse:\n def j(): pass\nfor _ in y:\n pass\nelse:\n def k(): pass\nmatch v:\n case 1:\n  def l(): pass\n",
    );
    assert_eq!(names(&f), ["f", "g", "h", "i", "j", "k", "l"]);
    let c = py("class A:\n if X:\n  def m(self): pass\n else:\n  def m(self): pass\n");
    assert_eq!(names(&c), ["A", "m", "m"]);
}

#[test]
fn string_text_counts_even_with_escapes() {
    let a = item("def f(x=\"abc\\n\"):\n return \"abc\\n\"\n", "f");
    let b = item("def f(x=\"xyz\\n\"):\n return \"abc\\n\"\n", "f");
    let c = item("def f(x=\"abc\\n\"):\n return \"xyz\\n\"\n", "f");
    assert_ne!(a.signature_hash, b.signature_hash);
    assert_ne!(a.body_hash, c.body_hash);
    let d = item("def f(x):\n return f\"a{{b}}{x}\"\n", "f");
    let e = item("def f(x):\n return f\"z{{b}}{x}\"\n", "f");
    assert_ne!(d.body_hash, e.body_hash);
}

#[test]
fn nested_scopes_keep_their_names_to_themselves() {
    assert!(
        item("def f():\n def g(cb):\n  cb()\n return g\n", "f")
            .calls
            .is_empty()
    );
    assert!(
        item("def f():\n class K:\n  def m(self):\n   self.go()\n", "f")
            .calls
            .is_empty()
    );
    // A lambda's parameter is not the function's.
    assert_eq!(
        item("def f():\n g = lambda helper: 0\n helper()\n", "f").calls,
        vec![p(&["helper"])]
    );
    // Nor a comprehension's variable.
    assert_eq!(
        item("def f(xs):\n [run for run in xs]\n run()\n", "f").calls,
        vec![p(&["run"])]
    );
    // An outer name is seen inside.
    assert!(
        item("def f(cb):\n return lambda: cb()\n", "f")
            .calls
            .is_empty()
    );
}

#[test]
fn match_captures_are_locals() {
    let f = item(
        "def f(x):\n match x:\n  case [handler, *rest]:\n   handler()\n   rest.pop()\n  case Foo(a=k) as h:\n   h.run()\n   k()\n  case {\"k\": v, **kw}:\n   v()\n   kw.get()\n  case m.CONST:\n   m.go()\n  case Point(px):\n   Point.make()\n",
        "f",
    );
    assert_eq!(f.calls, vec![p(&["m", "go"]), p(&["Point", "make"])]);
}

#[test]
fn receiver_rules() {
    let q = item(
        "class C:\n @builtins.staticmethod\n def m(x):\n  x.run()\n",
        "m",
    );
    assert!(q.calls.is_empty());
    let d = item("class C:\n def m(self=None):\n  self.go()\n", "m");
    assert_eq!(d.calls, vec![p(&["self", "go"])]);
    let t = item("def f[T](x):\n return T()\n", "f");
    assert!(t.calls.is_empty());
}

#[test]
fn reindenting_a_docstring_with_its_definition_changes_nothing() {
    let a = item("def f():\n    \"\"\"a\n    b\"\"\"\n", "f");
    let b = item("def f():\n        \"\"\"a\n        b\"\"\"\n", "f");
    assert_eq!(a.body_hash, b.body_hash);
    // Its words still count.
    let c = item("def f():\n    \"\"\"a\n    c\"\"\"\n", "f");
    assert_ne!(a.body_hash, c.body_hash);
}

#[test]
fn chained_class_attributes_are_names() {
    let a = item("class C:\n    x = y = 1\n", "C");
    let b = item("class C:\n    x = z = 1\n", "C");
    let c = item("class C:\n    x = y = 2\n", "C");
    assert_ne!(a.signature_hash, b.signature_hash);
    assert_eq!(a.signature_hash, c.signature_hash);
    assert_ne!(a.body_hash, c.body_hash);
}

#[test]
fn a_type_alias_rename_keeps_its_signature() {
    let a = item("type A = list[int]\n", "A");
    let b = item("type B = list[int]\n", "B");
    let c = item("type A = list[str]\n", "A");
    assert_eq!(a.signature_hash, b.signature_hash);
    assert_ne!(a.signature_hash, c.signature_hash);
}

#[test]
fn deep_nesting_is_not_quadratic() {
    let n = 8000;
    let src = format!(
        "x = {}{}{}\n",
        "[".repeat(n),
        "a, ".repeat(n),
        "]".repeat(n)
    );
    let t = std::time::Instant::now();
    let _ = Python.highlight(src.as_bytes());
    let _ = py(&src);
    // Was 1.5 s in release; generous for debug builds on CI.
    assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
}

// Regressions from the second review (#153).

#[test]
fn a_walrus_in_a_comprehension_binds_in_the_function() {
    let f = item(
        "def f(ps):\n if any((m := match(p)) for p in ps):\n  m.group()\n",
        "f",
    );
    assert_eq!(f.calls, vec![p(&["any"]), p(&["match"])]);
}

#[test]
fn class_body_names_are_not_seen_by_its_methods() {
    let f = item(
        "def f():\n class C:\n  helper = 1\n  def m(self):\n   helper()\n  x = helper()\n",
        "f",
    );
    // In `m`, `helper` is outside; in the class body it is the class's own.
    assert_eq!(f.calls, vec![p(&["helper"])]);
    let g = item(
        "def f():\n class C:\n  helper = 1\n  ys = [helper() for _ in range(3)]\n",
        "f",
    );
    assert_eq!(g.calls, vec![p(&["helper"]), p(&["range"])]);
}

#[test]
fn defaults_and_the_first_iterable_are_evaluated_outside() {
    for (src, want) in [
        ("def f():\n def g(make=make()): pass\n", "make"),
        ("def f():\n return lambda make=make(): 0\n", "make"),
        ("def f():\n return [items for items in items()]\n", "items"),
        ("def f():\n def g(x: T = T()) -> R: pass\n", "T"),
    ] {
        assert_eq!(item(src, "f").calls, vec![p(&[want])], "{src}");
    }
}

#[test]
fn global_hides_enclosing_function_names() {
    let f = item(
        "def f():\n helper = 1\n def g():\n  global helper\n  helper()\n",
        "f",
    );
    assert_eq!(f.calls, vec![p(&["helper"])]);
    // `nonlocal` still refers to the enclosing function's.
    let n = item(
        "def f():\n helper = 1\n def g():\n  nonlocal helper\n  helper()\n",
        "f",
    );
    assert!(n.calls.is_empty());
}

#[test]
fn class_and_concatenated_docstrings_ignore_reindenting() {
    let a = item(
        "class C:\n    \"\"\"Doc\n    more.\n    \"\"\"\n    x = 1\n",
        "C",
    );
    let b = item(
        "if x:\n        class C:\n            \"\"\"Doc\n            more.\n            \"\"\"\n            x = 1\n",
        "C",
    );
    assert_eq!(a.body_hash, b.body_hash);
    let c = item(
        "def f():\n    \"\"\"Doc\n    more.\n    \"\"\" \"x\"\n    return 1\n",
        "f",
    );
    let d = item(
        "def f():\n        \"\"\"Doc\n        more.\n        \"\"\" \"x\"\n        return 1\n",
        "f",
    );
    assert_eq!(c.body_hash, d.body_hash);
}

// Regressions from the third review (#153).

#[test]
fn class_scope_rules_hold_in_defaults_bases_and_nested_classes() {
    for (src, want) in [
        // A lambda or comprehension in a method's default does not see the class.
        (
            "def f():\n class C:\n  x = 1\n  def m(self, a=lambda: x()): pass\n",
            vec![p(&["x"])],
        ),
        (
            "def f():\n class C:\n  x = 1\n  def m(self, a=[x() for _ in y]): pass\n",
            vec![p(&["x"])],
        ),
        (
            "def f():\n class C:\n  x = 1\n  ys = [0 for a in [x() for b in c]]\n",
            vec![p(&["x"])],
        ),
        // A class's bases are read outside it.
        ("def f():\n class C(g()):\n  g = 1\n", vec![p(&["g"])]),
        // Class scopes do not nest.
        (
            "def f():\n class C:\n  x = 1\n  class D:\n   y = x()\n",
            vec![p(&["x"])],
        ),
        // A method's default does see the class.
        (
            "def f():\n class C:\n  x = 1\n  def m(self, a=x()): pass\n",
            vec![],
        ),
    ] {
        assert_eq!(item(src, "f").calls, want, "{src}");
    }
}

#[test]
fn a_comprehension_as_first_iterable_leaves_the_outer_one_intact() {
    assert!(
        item(
            "def f(c):\n return [1 for a in [b for b in c] if a()]\n",
            "f"
        )
        .calls
        .is_empty()
    );
}

#[test]
fn a_walrus_in_a_default_binds_outside() {
    assert!(
        item("def f():\n def g(a=(y := 1)): pass\n y()\n", "f")
            .calls
            .is_empty()
    );
    assert!(
        item("def f():\n h = lambda a=(y := 1): 0\n y()\n", "f")
            .calls
            .is_empty()
    );
}

#[test]
fn many_names_and_scopes_in_one_class_stay_linear() {
    let n = 20000;
    let mut src = "def f():\n class C:\n".to_owned();
    for i in 0..n {
        src.push_str(&format!("  a{i} = lambda: 0\n"));
    }
    for i in 0..n {
        src.push_str(&format!("  def m{i}(self): pass\n"));
    }
    let t = std::time::Instant::now();
    let _ = py(&src);
    // Was minutes; generous for debug builds on CI.
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
}

// Regressions from the fourth review (#153).

#[test]
fn type_parameters_are_seen_where_python_sees_them() {
    for src in [
        "def f():\n class C[T]:\n  x = T()\n",
        "def f():\n class C[T]:\n  def m(self): return T()\n",
        "def f():\n class C[T](B[T]):\n  def m(self): T.x()\n",
        "def f():\n class C[T](T.base()):\n  pass\n",
        "def f():\n def g[T](a: T.x()) -> T.y(): pass\n",
        "def f():\n type X = int\n X()\n",
    ] {
        assert!(
            item(src, "f").calls.is_empty(),
            "{src}: {:?}",
            item(src, "f").calls
        );
    }
    // A default is outside the type-parameter scope.
    assert_eq!(
        item("def f():\n def g[T](x=T()): pass\n", "f").calls,
        vec![p(&["T"])]
    );
    // A module-level generic class's methods, and its nested classes'.
    let f = py(
        "class C[T]:\n def m(self): return T()\n class D:\n  def n(self): return T()\nclass E:\n def o(self): return T()\n",
    );
    assert!(by_name(&f, "m").calls.is_empty());
    assert!(by_name(&f, "n").calls.is_empty());
    assert_eq!(by_name(&f, "o").calls, vec![p(&["T"])]);
}
