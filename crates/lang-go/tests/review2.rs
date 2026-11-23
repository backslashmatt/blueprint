//! Regressions from the second review of the Go analyser (#160): scopes and separators.
#![allow(clippy::unwrap_used, clippy::panic)]
use blueprint_lang_go::analyse;
use blueprint_model::lang::{FileFacts, Item};

fn go(src: &str) -> FileFacts {
    analyse(src.as_bytes())
}
fn item(src: &str, name: &str) -> Item {
    go(src)
        .items
        .into_iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("no {name}"))
}
fn p(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| (*x).to_owned()).collect()
}
fn calls(src: &str, name: &str) -> Vec<Vec<String>> {
    item(src, name).calls
}

// ---- hash: `;` dropped where it carries meaning ----
#[test]
fn h1_struct_embedded_vs_field() {
    // struct { x; y }: two embedded fields. struct { x y }: field x of type y.
    let a = item("package p\ntype S struct { x; y }\n", "S");
    let b = item("package p\ntype S struct { x y }\n", "S");
    assert_ne!(a.signature_hash, b.signature_hash);
}
#[test]
fn h2_switch_init_vs_tag() {
    // switch f(); {…}: init f(), tag true. switch f() {…}: tag f().
    let a = item("package p\nfunc g() { switch f(); { case true: } }\n", "g");
    let b = item("package p\nfunc g() { switch f() { case true: } }\n", "g");
    assert_ne!(a.body_hash, b.body_hash);
}
#[test]
fn h3_for_header_slots() {
    // init f(), cond g()  vs  cond f(), post g()
    let a = item("package p\nfunc h() { for f(); g(); {} }\n", "h");
    let b = item("package p\nfunc h() { for ; f(); g() {} }\n", "h");
    assert_ne!(a.body_hash, b.body_hash);
}
#[test]
fn h4_named_return_vs_return_value() {
    let a = item("package p\nfunc g() (x int) { return; f() }\n", "g");
    let b = item("package p\nfunc g() (x int) { return f() }\n", "g");
    assert_ne!(a.body_hash, b.body_hash);
}
// A comment after a trailing comma changes the hash (comments must not).
#[test]
fn h5_comment_after_trailing_comma() {
    let a = item("package p\nfunc f(\n\ta int,\n) {}\n", "f");
    let b = item("package p\nfunc f(\n\ta int, // why\n) {}\n", "f");
    assert_eq!(a.signature_hash, b.signature_hash);
}
#[test]
fn h6_generic_vs_array_trailing_comma() {
    let a = item("package p\ntype T[P *C,] struct{}\n", "T");
    let b = item("package p\ntype T[P *C] struct{}\n", "T");
    assert_ne!(a.signature_hash, b.signature_hash);
}

// ---- perf: next_sibling per comma ----
#[test]
fn perf1_wide_literal() {
    let n = 40_000;
    let mut s = String::from("package p\nfunc f() { _ = []int{");
    for i in 0..n {
        s.push_str(&format!("{i},"));
    }
    s.push_str("1} }\n");
    let t = std::time::Instant::now();
    let _ = go(&s);
    let el = t.elapsed();
    eprintln!("wide {n}: {el:?}");
    assert!(el.as_secs_f64() < 2.0, "{el:?}");
}

// ---- scopes ----
#[test]
fn s1_type_switch_alias_value_uses_receiver() {
    let c = calls(
        "package p\nfunc (n *N) K() {\n\tswitch n := n.inner().(type) {\n\tcase int:\n\t\t_ = n\n\t}\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["self", "inner"])]);
}
#[test]
fn s2_type_switch_alias_value_package_call() {
    let c = calls(
        "package p\nfunc K() {\n\tswitch next := next().(type) {\n\tcase int:\n\t\t_ = next\n\t}\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["next"])]);
}
#[test]
fn s3_if_else_scope() {
    let c = calls(
        "package p\nfunc K() {\n\tif g := 1; g > 0 {\n\t} else {\n\t\tg()\n\t}\n\tg()\n}\n",
        "K",
    );
    // inside else g is the int (local); after the if, g is the package func
    assert_eq!(c, vec![p(&["g"])]);
}
#[test]
fn s4_for_closure() {
    let c = calls(
        "package p\nfunc K() {\n\tfor i := 0; i < 3; i++ {\n\t\tdefer func() { i.M() }()\n\t}\n\ti.M()\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["i", "M"])]);
}
#[test]
fn s5_select_recv() {
    let c = calls(
        "package p\nfunc K(ch chan T) {\n\tselect {\n\tcase v := <-ch:\n\t\tv.M()\n\tdefault:\n\t\tv.N()\n\t}\n\tv.O()\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["v", "N"]), p(&["v", "O"])]);
}
#[test]
fn s6_case_scope() {
    let c = calls(
        "package p\nfunc K(x int) {\n\tswitch x {\n\tcase 1:\n\t\tv := 1\n\t\t_ = v\n\tcase 2:\n\t\tv.M()\n\t}\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["v", "M"])]);
}
#[test]
fn s7_var_in_block() {
    let c = calls(
        "package p\nfunc K() {\n\t{\n\t\tvar a, b = 1, 2\n\t\ta.M(); b.M()\n\t}\n\ta.M()\n\tvar c T = c.New()\n\tc.X()\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["a", "M"]), p(&["c", "New"])]);
}
#[test]
fn s8_labelled_goto() {
    let c = calls(
        "package p\nfunc K() {\nL:\n\tfor x := range xs {\n\t\tx.M()\n\t\tgoto L\n\t}\n\tx.N()\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["x", "N"])]);
}
#[test]
fn s9_nested_literals_go_defer() {
    let c = calls(
        "package p\nfunc K() {\n\tgo func(a int) {\n\t\tdefer func(b int) { a.M(); b.M(); c.M() }(a)\n\t\tb.Q()\n\t}(1)\n\ta.R()\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["c", "M"]), p(&["b", "Q"]), p(&["a", "R"])]);
}
#[test]
fn s10_const_iota_and_local_type() {
    let c = calls(
        "package p\nfunc K() {\n\tconst (\n\t\tA = iota\n\t\tB\n\t)\n\tB.M()\n\ttype U struct{}\n\tU.M()\n\tV.M()\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["V", "M"])]);
}
#[test]
fn s11_range_assign_no_bind() {
    let c = calls(
        "package p\nfunc K() {\n\tvar k int\n\tfor k = range xs {}\n\tfor j = range xs { j.M() }\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["j", "M"])]);
}
#[test]
fn s12_func_lit_self_ref() {
    let c = calls(
        "package p\nfunc K() {\n\tf := func() { f() }\n\tf()\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["f"])]);
}
#[test]
fn s13_receiver_shadow_in_case() {
    let c = calls(
        "package p\nfunc (s *S) K(x int) {\n\tswitch x {\n\tcase 1:\n\t\ts := 2\n\t\t_ = s\n\tcase 2:\n\t\ts.M()\n\t}\n\ts.N()\n}\n",
        "K",
    );
    assert_eq!(c, vec![p(&["self", "M"]), p(&["self", "N"])]);
}
#[test]
fn s14_crlf_no_newline() {
    let f = go("package p\r\n\r\ntype I interface { M() }");
    assert!(f.clean);
    let f = go("package p\r\nfunc f() {\r\n\tg()\r\n}\r");
    assert!(f.clean);
    assert_eq!(f.items[0].lines, (2, 4));
}
#[test]
fn perf2_deep_commas() {
    let d = 20_000;
    let mut s = String::from("package p\nfunc f() { _ = ");
    for _ in 0..d {
        s.push_str("g(1, ");
    }
    s.push('1');
    for _ in 0..d {
        s.push_str(", 2)");
    }
    s.push_str(" }\n");
    let t = std::time::Instant::now();
    let _ = go(&s);
    let el = t.elapsed();
    eprintln!("deep {d}: {el:?}");
    assert!(el.as_secs_f64() < 2.0, "{el:?}");
}
