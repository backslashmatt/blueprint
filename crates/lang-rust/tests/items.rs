//! The Rust analyser's items, hashes and robustness.

#![allow(clippy::panic)] // test code

use blueprint_lang_rust::analyse;
use blueprint_model::Visibility;
use blueprint_model::lang::{FileFacts, Item, ItemKind, Reference};

const SAMPLE: &str = r#"
use std::collections::{BTreeMap, btree_map::Entry as E};
use crate::l1::*;

/// A manifest.
pub struct Manifest<T> {
    name: String,
    extra: T,
}

pub(crate) enum Kind { A, B(u8) }

pub trait Analyse {
    fn analyse(&self) -> u8;
    fn twice(&self) -> u8 { self.analyse() * 2 }
}

impl<T> Manifest<T> {
    pub fn new(name: String, extra: T) -> Self {
        Self { name, extra }
    }
    fn private(&self) {}
}

impl<T> Analyse for Manifest<T> {
    fn analyse(&self) -> u8 { 1 }
}

fn helper(x: u32) -> u32 {
    fn nested() {}
    x + 1
}

mod inner {
    pub type Alias = u8;
    pub(super) fn deep() {}
}

macro_rules! m { () => { fn hidden() {} } }
"#;

fn by_name<'a>(f: &'a FileFacts, name: &str) -> &'a Item {
    f.items
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("no item {name} in {:?}", f.items))
}

#[test]
fn finds_types_functions_and_methods_in_source_order() {
    let f = analyse(SAMPLE.as_bytes());
    assert!(f.clean);
    let names: Vec<(&str, &[String])> = f
        .items
        .iter()
        .map(|i| (i.name.as_str(), i.module.as_slice()))
        .collect();
    let top: &[String] = &[];
    let inner: &[String] = &["inner".to_owned()];
    assert_eq!(
        names,
        vec![
            ("Manifest", top),
            ("Kind", top),
            ("Analyse", top),
            ("analyse", top),
            ("twice", top),
            ("new", top),
            ("private", top),
            ("analyse", top),
            ("helper", top),
            ("Alias", inner),
            ("deep", inner),
        ],
        "nested functions and macro bodies are not items"
    );
    let m = by_name(&f, "Manifest");
    assert_eq!(
        m.kind,
        ItemKind::Type {
            keyword: "struct".into()
        }
    );
    assert_eq!(m.lines, (6, 9));
    assert_eq!(m.visibility, Some(Visibility::Public));
    assert_eq!(by_name(&f, "Kind").visibility, Some(Visibility::Restricted));
    assert_eq!(by_name(&f, "helper").visibility, Some(Visibility::Private));
    assert_eq!(by_name(&f, "helper").body_line, Some(29));

    let methods: Vec<&ItemKind> = f
        .items
        .iter()
        .filter(|i| matches!(i.kind, ItemKind::Method { .. }))
        .map(|i| &i.kind)
        .collect();
    let method = |t: &str, tr: Option<&str>| ItemKind::Method {
        self_type: t.into(),
        r#trait: tr.map(Into::into),
    };
    assert_eq!(
        methods,
        vec![
            &method("Analyse", Some("Analyse")),
            &method("Analyse", Some("Analyse")),
            &method("Manifest", None),
            &method("Manifest", None),
            &method("Manifest", Some("Analyse")),
        ]
    );
    let trait_impl = f.items.iter().filter(|i| i.name == "analyse").nth(1);
    assert_eq!(trait_impl.map(|i| i.visibility), Some(None));
    assert_eq!(by_name(&f, "private").visibility, Some(Visibility::Private));
}

#[test]
fn use_paths_are_flattened() {
    let f = analyse(SAMPLE.as_bytes());
    let paths: Vec<String> = f
        .references
        .iter()
        .filter_map(|r| match r {
            Reference::Use { path, .. } => Some(path.join("::")),
            Reference::Impl { .. } => None,
        })
        .collect();
    assert_eq!(
        paths,
        [
            "std::collections::BTreeMap",
            "std::collections::btree_map::Entry",
            "crate::l1::*"
        ]
    );
}

fn item(src: &str, name: &str) -> Item {
    by_name(&analyse(src.as_bytes()), name).clone()
}

#[test]
fn whitespace_and_comments_change_no_hash() {
    let a = item("pub fn f(x: u8) -> u8 { x + 1 }", "f");
    let b = item(
        "pub  fn f( x : u8 )\n  -> u8 // returns\n{\n    /* add */ x+1\n}",
        "f",
    );
    assert_eq!(
        (a.signature_hash, a.body_hash),
        (b.signature_hash, b.body_hash)
    );
}

#[test]
fn body_and_signature_changes_are_told_apart_and_names_are_not_hashed() {
    let base = item("pub fn f(x: u8) -> u8 { x + 1 }", "f");
    let body = item("pub fn f(x: u8) -> u8 { x + 2 }", "f");
    assert_eq!(base.signature_hash, body.signature_hash);
    assert_ne!(base.body_hash, body.body_hash);
    let sig = item("pub fn f(x: u16) -> u8 { x + 1 }", "f");
    assert_ne!(base.signature_hash, sig.signature_hash);
    assert_eq!(base.body_hash, sig.body_hash);
    let vis = item("fn f(x: u8) -> u8 { x + 1 }", "f");
    assert_ne!(base.signature_hash, vis.signature_hash);
    let renamed = item("pub fn g(x: u8) -> u8 { x + 1 }", "g");
    assert_eq!(
        (base.signature_hash, base.body_hash),
        (renamed.signature_hash, renamed.body_hash)
    );
    let field = item("struct S { a: u8 }", "S");
    let field2 = item("struct S { a: u16 }", "S");
    assert_ne!(
        field.signature_hash, field2.signature_hash,
        "fields are surface"
    );
}

#[test]
fn a_damaged_file_keeps_what_could_be_read() {
    let f = analyse(b"pub fn good() {}\nfn broken( {\npub struct After;\n");
    assert!(!f.clean);
    assert!(f.items.iter().any(|i| i.name == "good"));
}

#[test]
fn hostile_input_never_panics() {
    let deep = "mod a {".repeat(20_000);
    let f = analyse(deep.as_bytes());
    assert!(!f.clean);
    let deep_use = format!("use {}x;", "a::{".repeat(5_000));
    let _ = analyse(deep_use.as_bytes());
    let _ = analyse(&[0xff, 0xfe, 0x00, b'{', b'}']);
    // Deterministic pseudo-random bytes and token soup.
    let tokens = [
        "fn", "impl", "mod", "{", "}", "(", ")", "<", ">", "::", "pub", "x", ";", "for", "trait",
        "\"", "'", "/*", "*/", "\n", "#[", "]",
    ];
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    for _ in 0..300 {
        let mut s = String::new();
        for _ in 0..200 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let i = usize::try_from(state % tokens.len() as u64).unwrap_or(0);
            s.push_str(tokens[i]);
            s.push(' ');
        }
        let _ = analyse(s.as_bytes());
    }
}

#[test]
fn deep_inline_modules_stop_at_the_limit() {
    let src = format!("{}fn f() {{}}{}", "mod a { ".repeat(100), "}".repeat(100));
    let f = analyse(src.as_bytes());
    assert!(!f.clean, "modules beyond the limit are reported as unread");
    assert!(f.items.is_empty());
}

#[test]
fn bodiless_items_have_no_body_hash() {
    assert_eq!(item("struct A;", "A").body_hash, None);
    assert_eq!(item("struct B { x: u8 }", "B").body_hash, None);
    assert_eq!(item("type T = u8;", "T").body_hash, None);
    assert_eq!(item("trait M {}", "M").body_hash, None);
    assert_eq!(item("trait M { fn f(&self); }", "f").body_hash, None);
    assert!(item("trait M { fn f(&self); }", "M").body_hash.is_some());
}

#[test]
fn a_traits_associated_items_are_its_surface() {
    let base = item("pub trait Q { fn f(&self); }", "Q");
    let assoc = item("pub trait Q { type Item; fn f(&self); }", "Q");
    assert_ne!(base.signature_hash, assoc.signature_hash);
    assert_eq!(base.body_hash, assoc.body_hash);
    let method = item("pub trait Q { fn f(&self); fn g(&self); }", "Q");
    assert_eq!(base.signature_hash, method.signature_hash);
    assert_ne!(base.body_hash, method.body_hash);
}

#[test]
fn an_impl_header_is_part_of_its_methods_surface() {
    let base = item("impl<T: Clone> X<T> { pub fn m(&self) {} }", "m");
    let bound = item("impl<T: Copy> X<T> { pub fn m(&self) {} }", "m");
    let clause = item("impl<T> X<T> where T: Copy { pub fn m(&self) {} }", "m");
    assert_ne!(base.signature_hash, bound.signature_hash);
    assert_ne!(base.signature_hash, clause.signature_hash);
    assert_eq!(base.body_hash, bound.body_hash);
}

#[test]
fn attributes_are_surface_and_part_of_the_span() {
    let base = item("#[derive(Clone)]\npub struct S;", "S");
    let more = item("#[derive(Clone, Copy)]\npub struct S;", "S");
    assert_ne!(base.signature_hash, more.signature_hash);
    assert_eq!(base.lines, (1, 2));
    let doc = item("/// Docs.\n#[derive(Clone)]\npub struct S;", "S");
    assert_eq!(
        doc.signature_hash, base.signature_hash,
        "doc comments are not surface"
    );
}

#[test]
fn extern_functions_are_items() {
    let f = analyse(b"extern \"C\" { pub fn ext(x: i32) -> i32; }");
    assert_eq!(by_name(&f, "ext").kind, ItemKind::Function);
    assert_eq!(by_name(&f, "ext").body_hash, None);
}

#[test]
fn use_edge_cases() {
    let paths = |src: &str| -> Vec<String> {
        analyse(src.as_bytes())
            .references
            .iter()
            .filter_map(|r| match r {
                Reference::Use { path, .. } => Some(path.join("::")),
                Reference::Impl { .. } => None,
            })
            .collect()
    };
    assert_eq!(paths("use a::{self, b};"), ["a", "a::b"]);
    assert_eq!(paths("use a::/*c*/b;"), ["a::b"]);
    assert_eq!(paths("use ::std::x;"), ["std::x"]);
    assert_eq!(
        paths("mod m { use inner::z; }\nuse after::w;"),
        ["inner::z", "after::w"],
        "source order, across inline modules"
    );
    let deep = format!("use {}x{};", "a::{".repeat(100), "}".repeat(100));
    assert!(
        !analyse(deep.as_bytes()).clean,
        "a cut-off use group is reported"
    );
}

#[test]
fn pub_self_is_private() {
    assert_eq!(
        item("pub(self) fn f() {}", "f").visibility,
        Some(Visibility::Private)
    );
    assert_eq!(
        item("pub(crate) fn f() {}", "f").visibility,
        Some(Visibility::Restricted)
    );
}

#[test]
fn impl_names_are_spelt_from_tokens() {
    let kind = |src: &str| analyse(src.as_bytes()).items[0].kind.clone();
    let method = |t: &str, tr: Option<&str>| ItemKind::Method {
        self_type: t.into(),
        r#trait: tr.map(Into::into),
    };
    assert_eq!(
        kind("impl Tr for (u8,u16) { fn f() {} }"),
        method("(u8,u16)", Some("Tr"))
    );
    assert_eq!(
        kind("impl Tr for ( u8 , u16 ) { fn f() {} }"),
        method("(u8,u16)", Some("Tr"))
    );
    assert_eq!(
        kind("impl<'a> Tr for (&'a u8, Vec<u8>) { fn f() {} }"),
        method("(&u8,Vec<u8>)", Some("Tr"))
    );
    assert_eq!(kind("impl dyn Tr { fn f() {} }"), method("dyn Tr", None));
    assert_eq!(
        kind("impl std::convert::From<u16> for X { fn from(_: u16) -> X { X } }"),
        method("X", Some("From<u16>"))
    );
}

#[test]
fn impls_and_aliases_are_references() {
    let f = analyse(
        b"use a::B as C;\nimpl<T> crate::x::Walk<T> for Graph<T> {}\nimpl Graph<u8> { fn f() {} }\n",
    );
    assert_eq!(
        f.references,
        vec![
            Reference::Use {
                path: vec!["a".into(), "B".into()],
                alias: Some("C".into()),
                module: vec![],
            },
            Reference::Impl {
                self_type: vec!["Graph".into()],
                r#trait: vec!["crate".into(), "x".into(), "Walk".into()],
                module: vec![],
            },
        ],
        "an inherent impl implements nothing"
    );
}

#[test]
fn functions_record_the_paths_they_call() {
    let f = analyse(
        b"fn f() {\n    helper();\n    a::b::run(1);\n    Vec::<u8>::new();\n    Self::x();\n    helper();\n    x.method();\n    println!(\"{}\", inner());\n    let g = || later();\n    fn nested() { hidden(); }\n}\nstruct S;\n",
    );
    let calls: Vec<String> = by_name(&f, "f")
        .calls
        .iter()
        .map(|c| c.join("::"))
        .collect();
    assert_eq!(
        calls,
        ["helper", "a::b::run", "Vec::new", "Self::x", "later"]
    );
    assert!(by_name(&f, "S").calls.is_empty());
}
