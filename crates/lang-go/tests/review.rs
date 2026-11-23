//! Regressions from the review of the Go analyser (#160).

#![allow(clippy::unwrap_used, clippy::panic)] // test code
use blueprint_lang_go::{Go, analyse};
use blueprint_model::lang::{FileFacts, Item, LanguageAnalyser, Reference};

fn go(src: &str) -> FileFacts {
    analyse(src.as_bytes())
}
fn item(src: &str, name: &str) -> Item {
    go(src)
        .items
        .into_iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("no {name} in {src}"))
}
fn p(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| (*x).to_owned()).collect()
}
fn calls(src: &str, name: &str) -> Vec<Vec<String>> {
    item(src, name).calls
}

// 1. Index-expression callee is read as a call of the collection (doc says None).
#[test]
fn d1_index_call_invents_call() {
    let c = calls(
        "package p\nvar handlers []func()\nfunc f() { handlers[0](); pkg.Table[\"k\"]() }\n",
        "f",
    );
    assert_eq!(c, Vec::<Vec<String>>::new(), "{c:?}");
}

// 2. Statement separators: `;` vs newline (gofmt) changes body hash.
#[test]
fn d2_semicolon_vs_newline_body_hash() {
    let a = item("package p\nfunc f() { a(); b() }\n", "f");
    let b = item("package p\nfunc f() {\n\ta()\n\tb()\n}\n", "f");
    assert_eq!(a.body_hash, b.body_hash);
}
#[test]
fn d2b_struct_fields_separators() {
    let a = item("package p\ntype S struct { x int; y int }\n", "S");
    let b = item("package p\ntype S struct {\n\tx int\n\ty int\n}\n", "S");
    assert_eq!(a.signature_hash, b.signature_hash);
}
#[test]
fn d2c_trailing_comma_params() {
    let a = item("package p\nfunc f(a int, b int) {}\n", "f");
    let b = item("package p\nfunc f(\n\ta int,\n\tb int,\n) {}\n", "f");
    assert_eq!(a.signature_hash, b.signature_hash);
}

// 3. Flat locals: a closure parameter / inner-block local drops real calls elsewhere.
#[test]
fn d3_closure_param_drops_package_call() {
    let c = calls(
        "package p\nimport \"fmt\"\nfunc f() {\n\tgo func(fmt int) { _ = fmt }(1)\n\tfmt.Println()\n}\n",
        "f",
    );
    assert_eq!(c, vec![p(&["fmt", "Println"])]);
}
#[test]
fn d3b_later_decl_drops_earlier_call() {
    let c = calls(
        "package p\nfunc f() {\n\thelper()\n\tif true {\n\t\thelper := 1\n\t\t_ = helper\n\t}\n}\n",
        "f",
    );
    assert_eq!(c, vec![p(&["helper"])]);
}

// 4. Receiver shadowed by a local: invented self call.
#[test]
fn d4_shadowed_receiver_becomes_self() {
    let c = calls(
        "package p\nfunc (t T) M(ts []U) {\n\tfor _, t := range ts {\n\t\tt.Close()\n\t}\n}\n",
        "M",
    );
    assert!(!c.contains(&p(&["self", "Close"])), "{c:?}");
}
#[test]
fn d4b_closure_param_shadows_receiver() {
    let c = calls(
        "package p\nfunc (t T) M() {\n\tgo func(t U) { t.Close() }(u)\n}\n",
        "M",
    );
    assert!(!c.contains(&p(&["self", "Close"])), "{c:?}");
}

// 5. Generic receiver's type parameters are not bound.
#[test]
fn d5_receiver_type_params_not_bound() {
    let c = calls(
        "package p\nfunc (l *List[T]) Zero() T { return T(0) }\n",
        "Zero",
    );
    assert!(c.is_empty(), "{c:?}");
    // Compare: function type params are bound.
    assert!(calls("package p\nfunc Zero[T ~int]() T { return T(0) }\n", "Zero").is_empty());
}

// 6. Local type declarations: conversion read as a call.
#[test]
fn d6_local_type_conversion() {
    let c = calls(
        "package p\nfunc f(x int) {\n\ttype handler int\n\t_ = handler(x)\n}\n",
        "f",
    );
    assert!(c.is_empty(), "{c:?}");
}

// 7. Generic call with several type args, unqualified/qualified as call_expression? (type_instantiation)
#[test]
fn d7_generic_calls() {
    let c = calls(
        "package p\nfunc f() { F[int](x); pkg.G[int, string](x); H[int, string](y); a.b.M[int](z) }\n",
        "f",
    );
    assert_eq!(
        c,
        vec![p(&["F"]), p(&["pkg", "G"]), p(&["H"]), p(&["a", "b", "M"])],
        "{c:?}"
    );
}

// 8. Missing trailing newline is not an error.
#[test]
fn d8_no_trailing_newline_is_clean() {
    assert!(go("package p\nfunc f() {}").clean);
    assert!(go("package p\ntype I interface { M() }").clean);
}

// 9. Interface type-set element `int` / `comparable` is not embedding.
#[test]
fn d9_interface_typeset_not_embedding() {
    let f = go("package p\ntype Number interface { int }\ntype C interface { comparable }\n");
    assert!(f.references.is_empty(), "{:?}", f.references);
}

// 10. receive assignment (not declaration) binds.
#[test]
fn d10_receive_assignment_does_not_bind() {
    let c = calls(
        "package p\nvar cb func()\nfunc f(ch chan func()) {\n\tcb()\n\tselect {\n\tcase cb = <-ch:\n\t}\n}\n",
        "f",
    );
    assert_eq!(c, vec![p(&["cb"])]);
}

// 11. init functions, several.
#[test]
fn d11_inits() {
    let f = go("package p\nfunc init() { a() }\nfunc init() { b() }\n");
    let n = f.items.iter().filter(|i| i.name == "init").count();
    println!("init count {n}");
}

// 12. bodyless func.
#[test]
fn d12_bodyless() {
    let i = item("package p\n//go:noescape\nfunc add(a, b int) int\n", "add");
    assert_eq!(i.body_hash, None);
    assert!(i.calls.is_empty());
}

// 13. Import with comment, raw import.
#[test]
fn d13_imports() {
    let f = go(
        "package p\nimport (\n\t// c\n\t\"fmt\" // c2\n\tx /* c */ \"a/b\"\n)\nimport `raw/path`\n",
    );
    let v: Vec<_> = f
        .references
        .iter()
        .map(|r| match r {
            Reference::Use { path, alias, .. } => format!("{path:?} {alias:?}"),
            _ => String::new(),
        })
        .collect();
    assert_eq!(
        v,
        [
            "[\"fmt\"] None",
            "[\"a/b\"] Some(\"x\")",
            "[\"raw/path\"] None"
        ]
    );
}

// 14. Strings: escapes vs raw equal text, different hashes - expected (tokens).
#[test]
fn d14_string_hash() {
    let a = item("package p\nfunc f() string { return \"a\\tb\" }\n", "f");
    let b = item("package p\nfunc f() string { return \"a\\nb\" }\n", "f");
    assert_ne!(a.body_hash, b.body_hash);
    let a = item("package p\ntype S struct { x int `json:\"a\"` }\n", "S");
    let b = item("package p\ntype S struct { x int `json:\"b\"` }\n", "S");
    assert_ne!(a.signature_hash, b.signature_hash);
    let a = item("package p\nfunc f(a ...int) {}\n", "f");
    let b = item("package p\nfunc f(a []int) {}\n", "f");
    assert_ne!(a.signature_hash, b.signature_hash);
}

// 15. highlight on deep / odd input: sorted, disjoint, in range.
#[test]
fn d15_highlight() {
    let deep =
        "package p\nvar x = ".to_owned() + &"(".repeat(20000) + "1" + &")".repeat(20000) + "\n";
    let t = std::time::Instant::now();
    for src in [
        deep.as_str(),
        "package p\nfunc f() { s := \"\\u00e9\\\"\" + `x`; /* c */ x.y.z() }",
        "\u{feff}package p\n",
    ] {
        let h = Go.highlight(src.as_bytes());
        assert!(h.windows(2).all(|w| w[0].end <= w[1].start));
        assert!(h.iter().all(|t| t.end as usize <= src.len()));
    }
    println!("hl {:?}", t.elapsed());
}

// 16. Deep nesting in analyse: cost.
#[test]
fn d16_deep_analyse() {
    let deep = "package p\nfunc f() { x := ".to_owned()
        + &"func() int { return ".repeat(3000)
        + "1"
        + &" }()".repeat(3000)
        + "\n_ = x }\n";
    let t = std::time::Instant::now();
    let _ = go(&deep);
    println!("deep {:?}", t.elapsed());
}

// 17. method value / method expression
#[test]
fn d17_method_value() {
    let c = calls(
        "package p\nfunc f(t T) {\n\tg := t.M\n\tg()\n\tT.M(t)\n}\n",
        "f",
    );
    assert_eq!(c, vec![p(&["T", "M"])]);
}

// 18. Embedded forms.
#[test]
fn d18_embedded() {
    let f = go(
        "package p\ntype S struct {\n\t*Base\n\tpkg.T\n\tG[int]\n\t*pkg.H[K]\n\tx, y int\n\tinner struct{ Hidden }\n}\n",
    );
    let v: Vec<_> = f
        .references
        .iter()
        .map(|r| match r {
            Reference::Impl { r#trait, .. } => r#trait.join("."),
            _ => String::new(),
        })
        .collect();
    assert_eq!(v, ["Base", "pkg.T", "G", "pkg.H"]);
}

#[test]
fn d2d_composite_trailing_comma() {
    let a = item("package p\nfunc f() { _ = []int{1, 2} }\n", "f");
    let b = item(
        "package p\nfunc f() {\n\t_ = []int{\n\t\t1,\n\t\t2,\n\t}\n}\n",
        "f",
    );
    assert_eq!(a.body_hash, b.body_hash);
}
#[test]
fn d19_scope_receiver_param_named_result() {
    assert!(calls("package p\nfunc f() (fmt int) { fmt.X(); return }\n", "f").is_empty());
    assert!(calls("package p\nfunc f() { if g := h; g != nil { g() } }\n", "f").is_empty());
    assert!(
        calls(
            "package p\nfunc f(x any) { switch g := x.(type) { default: g() } }\n",
            "f"
        )
        .is_empty()
    );
    assert!(calls("package p\nfunc f(xs ...func()) { xs[0]() }\n", "f").is_empty());
}
