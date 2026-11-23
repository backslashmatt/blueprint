//! The Go analyser's facts.

use blueprint_lang_go::{Go, analyse};
use blueprint_model::lang::{FileFacts, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};

fn go(src: &str) -> FileFacts {
    analyse(src.as_bytes())
}

fn by_name<'a>(f: &'a FileFacts, name: &str) -> &'a Item {
    let found = f.items.iter().find(|i| i.name == name);
    found.unwrap_or_else(|| unreachable!("no item {name} in {:#?}", f.items))
}

fn item(src: &str, name: &str) -> Item {
    by_name(&go(src), name).clone()
}

fn p(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| (*x).to_owned()).collect()
}

const SAMPLE: &str = r#"package app

import (
	"fmt"
	str "strings"
	. "math"
	_ "embed"
	"net/http"
)

// T is a thing.
type T[K comparable] struct {
	Base
	*pkg.Other
	x int `json:"x"`
}

type I interface {
	Base
	M(a int) error
}

type A = int

type (
	D float64
	e struct{}
)

func (t *T[K]) M(a int) (err error) {
	b := 1
	for i, v := range t.xs {
		v.Run(i)
	}
	go func(c int) { c.Go() }(b)
	t.helper()
	fmt.Println(a)
	http.Get("x")
	return pkg.F[int](b)
}

func F[V any](v V) V { return v }

func helper() {}
"#;

#[test]
fn finds_types_functions_and_methods_in_source_order() {
    let f = go(SAMPLE);
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
    use Visibility::{Public, Restricted};
    assert_eq!(
        got,
        vec![
            ("T", "struct".into(), Some(Public)),
            ("I", "interface".into(), Some(Public)),
            ("A", "alias".into(), Some(Public)),
            ("D", "type".into(), Some(Public)),
            ("e", "struct".into(), Some(Restricted)),
            ("M", "method of T".into(), Some(Public)),
            ("F", "function".into(), Some(Public)),
            ("helper", "function".into(), Some(Restricted)),
        ]
    );
    assert!(f.items.iter().all(|i| i.module.is_empty()));
    assert_eq!(by_name(&f, "T").body_hash, None);
}

#[test]
fn imports_and_embedding_are_references() {
    let f = go(SAMPLE);
    let refs: Vec<String> = f
        .references
        .iter()
        .map(|r| match r {
            Reference::Use { path, alias, .. } => format!("use {} {alias:?}", path.join("|")),
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
            "use fmt None",
            "use strings Some(\"str\")",
            "use math Some(\".\")",
            "use embed Some(\"_\")",
            "use net/http None",
            "T : Base true",
            "T : pkg.Other true",
            "I : Base true",
        ]
    );
}

#[test]
fn calls_skip_locals_and_name_the_receiver_self() {
    let f = go(SAMPLE);
    assert_eq!(
        by_name(&f, "M").calls,
        vec![
            p(&["self", "helper"]),
            p(&["fmt", "Println"]),
            p(&["http", "Get"]),
            p(&["pkg", "F"]),
        ]
    );
    assert!(by_name(&f, "F").calls.is_empty());
    let g = item(
        "package p\nfunc g(cb func()) {\n\tcb()\n\tvar x T\n\tx.Do()\n\tswitch y := v.(type) {\n\tcase int:\n\t\ty.Z()\n\t}\n\tGeneric[int]()\n\tdone()\n}\n",
        "g",
    );
    assert_eq!(g.calls, vec![p(&["Generic"]), p(&["done"])]);
    // A blank or unnamed receiver binds nothing.
    let m = item("package p\nfunc (T) M() { other() }\n", "M");
    assert_eq!(m.calls, vec![p(&["other"])]);
}

#[test]
fn body_changes_keep_the_signature_and_signature_changes_show() {
    let base = "package p\nfunc F(a int) int {\n\treturn a\n}\n";
    let f0 = item(base, "F");
    let same = item(
        "package p\n\n// c\nfunc F(a int) int { return a /* c */ }\n",
        "F",
    );
    assert_eq!(f0.signature_hash, same.signature_hash);
    assert_eq!(f0.body_hash, same.body_hash);
    for changed in [
        "package p\nfunc F(a int64) int {\n\treturn a\n}\n",
        "package p\nfunc F(a int) (int, error) {\n\treturn a\n}\n",
        "package p\nfunc F[T any](a int) int {\n\treturn a\n}\n",
    ] {
        let c = item(changed, "F");
        assert_ne!(f0.signature_hash, c.signature_hash, "{changed}");
        assert_eq!(f0.body_hash, c.body_hash, "{changed}");
    }
    let body = item("package p\nfunc F(a int) int {\n\treturn a + 1\n}\n", "F");
    assert_ne!(f0.body_hash, body.body_hash);
    // String text counts, escapes or not.
    let s1 = item("package p\nfunc F() string { return \"abc\\n\" }\n", "F");
    let s2 = item("package p\nfunc F() string { return \"xyz\\n\" }\n", "F");
    assert_ne!(s1.body_hash, s2.body_hash);
    // A method's receiver is signature.
    let r1 = item("package p\nfunc (t T) M() {}\n", "M");
    let r2 = item("package p\nfunc (t *T) M() {}\n", "M");
    assert_ne!(r1.signature_hash, r2.signature_hash);
}

#[test]
fn struct_fields_and_tags_are_signature_and_renames_keep_it() {
    let a = item("package p\ntype S struct { x int }\n", "S");
    let b = item("package p\ntype R struct { x int }\n", "R");
    assert_eq!(a.signature_hash, b.signature_hash);
    for changed in [
        "package p\ntype S struct { y int }\n",
        "package p\ntype S struct { x int `json:\"x\"` }\n",
        "package p\ntype S struct { x int; Base }\n",
    ] {
        assert_ne!(
            a.signature_hash,
            item(changed, "S").signature_hash,
            "{changed}"
        );
    }
}

#[test]
fn handles_go_files_only() {
    let path = |s: &str| RepoPath::parse(s).unwrap_or_else(|_| unreachable!());
    assert!(Go.handles(&path("cmd/main.go")));
    assert!(Go.handles(&path("x_test.go")));
    assert!(!Go.handles(&path("go.mod")));
    assert!(!Go.handles(&path("go.sum")));
}

#[test]
fn broken_and_hostile_input_never_panics() {
    let deep = "package p\nfunc f() {\n".to_owned() + &"{".repeat(5000) + &"}".repeat(5000) + "}\n";
    let chain = "package p\nfunc f() { a".to_owned() + &".b".repeat(5000) + "() }\n";
    for src in [
        "package p\nfunc f( {\n",
        "type T struct {",
        "\u{feff}package p",
        "\u{0}\u{1}",
        &deep,
        &chain,
    ] {
        let _ = go(src);
        let _ = Go.highlight(src.as_bytes());
    }
    let f = go("package p\nfunc ok() { a() }\nfunc broken( {\n");
    assert!(!f.clean);
    assert!(f.items.iter().any(|i| i.name == "ok"), "{:#?}", f.items);
}
