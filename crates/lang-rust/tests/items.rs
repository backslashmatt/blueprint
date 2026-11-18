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
        .map(|r| match r {
            Reference::Use { path, .. } => path.join("::"),
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
