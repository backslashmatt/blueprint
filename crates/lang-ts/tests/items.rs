//! The TypeScript/JavaScript analyser's facts (#136).

use blueprint_lang_ts::{Tsx, TypeScript};
use blueprint_model::lang::{FileFacts, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};

fn ts(src: &str) -> FileFacts {
    TypeScript.analyse(&path("a.ts"), src.as_bytes())
}

fn path(p: &str) -> RepoPath {
    RepoPath::parse(p).unwrap_or_else(|_| unreachable!())
}

fn by_name<'a>(f: &'a FileFacts, name: &str) -> &'a Item {
    let found = f.items.iter().find(|i| i.name == name);
    found.unwrap_or_else(|| unreachable!("no item {name} in {:#?}", f.items))
}

fn item(src: &str, name: &str) -> Item {
    by_name(&ts(src), name).clone()
}

const SAMPLE: &str = r#"import D, { a as b, c } from "./x";
import * as ns from "lib";
export class K<T> extends ns.Base<T> implements I {
  private x: number = 1;
  protected h = (a: number) => a;
  #y() {}
  static m(): void { f(); }
}
export interface I extends A { f(x: number): string }
enum E { A, B }
export type T = { a: 1 };
export function f(x: number): number { return x; }
export const g = async (y) => { b(); ns.run(y); y(); };
namespace N.M { export function h() {} }
"#;

#[test]
fn finds_types_functions_and_methods_in_source_order() {
    let f = ts(SAMPLE);
    assert!(f.clean);
    let got: Vec<(&str, &ItemKind, Option<Visibility>, Vec<String>)> = f
        .items
        .iter()
        .map(|i| (i.name.as_str(), &i.kind, i.visibility, i.module.clone()))
        .collect();
    let class = |k: &str| ItemKind::Type { keyword: k.into() };
    let method = ItemKind::Method {
        self_type: "K".into(),
        r#trait: None,
    };
    let none = Vec::<String>::new;
    use Visibility::{Private, Public, Restricted};
    assert_eq!(
        got,
        vec![
            ("K", &class("class"), Some(Public), none()),
            ("h", &method, Some(Restricted), none()),
            ("#y", &method, Some(Private), none()),
            ("m", &method, Some(Public), none()),
            ("I", &class("interface"), Some(Public), none()),
            ("E", &class("enum"), Some(Private), none()),
            ("T", &class("type"), Some(Public), none()),
            ("f", &ItemKind::Function, Some(Public), none()),
            ("g", &ItemKind::Function, Some(Public), none()),
            (
                "h",
                &ItemKind::Function,
                Some(Public),
                vec!["N".into(), "M".into()]
            ),
        ]
    );
    // The class spans its `export`; its methods are its body.
    let k = by_name(&f, "K");
    assert_eq!(k.lines, (3, 8));
    assert!(k.body_hash.is_some());
    assert_eq!(by_name(&f, "I").body_hash, None);
}

#[test]
fn imports_and_heritage_are_references() {
    let f = ts(SAMPLE);
    let use_ = |path: &[&str], alias: Option<&str>| Reference::Use {
        path: path.iter().map(|s| (*s).to_owned()).collect(),
        alias: alias.map(str::to_owned),
        module: Vec::new(),
    };
    let impl_ = |ty: &str, tr: &[&str]| Reference::Impl {
        self_type: vec![ty.into()],
        r#trait: tr.iter().map(|s| (*s).to_owned()).collect(),
        module: Vec::new(),
    };
    assert_eq!(
        f.references,
        vec![
            use_(&["./x", "default"], Some("D")),
            use_(&["./x", "a"], Some("b")),
            use_(&["./x", "c"], None),
            use_(&["lib", "*"], Some("ns")),
            impl_("K", &["ns", "Base"]),
            impl_("K", &["I"]),
            impl_("I", &["A"]),
        ]
    );
}

#[test]
fn other_module_forms_are_references() {
    let f = Tsx.analyse(
        &path("a.js"),
        br#"export * from "./all";
export { q as r } from "./q";
const w = require("./w");
const { z } = require("./z");
import y = require("./y");
async function load() { await import("./lazy"); w.go(); }
"#,
    );
    let paths: Vec<Vec<String>> = f
        .references
        .iter()
        .filter_map(|r| match r {
            Reference::Use { path, .. } => Some(path.clone()),
            Reference::Impl { .. } => None,
        })
        .collect();
    let p = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![
            p(&["./all", "*"]),
            p(&["./q", "q"]),
            p(&["./w"]),
            p(&["./z"]),
            p(&["./y"]),
            p(&["./lazy"]),
        ]
    );
    assert_eq!(by_name(&f, "load").calls, vec![p(&["./w", "go"])]);
}

#[test]
fn calls_resolve_imports_and_skip_locals() {
    let f = ts(SAMPLE);
    let p = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    // `b` is `a` from "./x"; `ns.run` is "lib"'s `run`; `y` is a parameter.
    assert_eq!(
        by_name(&f, "g").calls,
        vec![p(&["./x", "a"]), p(&["lib", "run"])]
    );
    assert_eq!(by_name(&f, "m").calls, vec![p(&["f"])]);
    let local = item(
        "function f(cb, { k, v: w }, d = other) { const z = 1; function inner() { hidden(); }\n  cb(); k(); w(); other(); inner(); [1].map((n) => n2(n)); }",
        "f",
    );
    assert_eq!(local.calls, vec![p(&["other"]), p(&["n2"])]);
}

#[test]
fn whitespace_and_comments_change_no_hash() {
    let a = item(
        "export function f(x: number): number { return x + 1; }",
        "f",
    );
    let b = item(
        "export  function f( x : number )\n  : number // returns\n{\n  /* add */ return x+1;\n}",
        "f",
    );
    assert_eq!(a.signature_hash, b.signature_hash);
    assert_eq!(a.body_hash, b.body_hash);
}

#[test]
fn body_and_signature_changes_are_told_apart_and_names_are_not_hashed() {
    let base = item("export function f(x: number) { return x + 1; }", "f");
    let body = item("export function f(x: number) { return x + 2; }", "f");
    assert_eq!(base.signature_hash, body.signature_hash);
    assert_ne!(base.body_hash, body.body_hash);
    let sig = item("export function f(x: string) { return x + 1; }", "f");
    assert_ne!(base.signature_hash, sig.signature_hash);
    assert_eq!(base.body_hash, sig.body_hash);
    let unexported = item("function f(x: number) { return x + 1; }", "f");
    assert_ne!(base.signature_hash, unexported.signature_hash);
    let renamed = item("export function g(x: number) { return x + 1; }", "g");
    assert_eq!(base.signature_hash, renamed.signature_hash);
    assert_eq!(base.body_hash, renamed.body_hash);
    // The same for an arrow function held by a `const`.
    let arrow = item("export const f = (x: number) => x + 1;", "f");
    let arrow_body = item("export const f = (x: number) => x + 2;", "f");
    let arrow_let = item("export let f = (x: number) => x + 1;", "f");
    assert_eq!(arrow.signature_hash, arrow_body.signature_hash);
    assert_ne!(arrow.body_hash, arrow_body.body_hash);
    assert_ne!(arrow.signature_hash, arrow_let.signature_hash);
}

#[test]
fn a_classes_fields_are_its_surface_and_its_methods_its_body() {
    let base = item("export class C { x: number = 1; m() { return 1; } }", "C");
    let typed = item("export class C { x: string = 1; m() { return 1; } }", "C");
    let init = item("export class C { x: number = 2; m() { return 1; } }", "C");
    let method = item("export class C { x: number = 1; m() { return 2; } }", "C");
    // A field's type is surface; its initial value, like a method, is body.
    assert_ne!(base.signature_hash, typed.signature_hash);
    assert_eq!(base.signature_hash, init.signature_hash);
    assert_ne!(base.body_hash, init.body_hash);
    assert_eq!(base.signature_hash, method.signature_hash);
    assert_ne!(base.body_hash, method.body_hash);
    let block = item("export class C { static { a(); } }", "C");
    let block2 = item("export class C { static { b(); } }", "C");
    assert_eq!(block.signature_hash, block2.signature_hash);
    // A function-valued field's type is the class's surface.
    let h = item(
        "export class C { h: (a: number) => void = (a) => {}; }",
        "C",
    );
    let h2 = item(
        "export class C { h: (a: string) => void = (a) => {}; }",
        "C",
    );
    assert_ne!(h.signature_hash, h2.signature_hash);
    // A class's type parameters are part of each method's surface.
    let m = item("class C<T> { m() {} }", "m");
    let bound = item("class C<T extends string> { m() {} }", "m");
    assert_ne!(m.signature_hash, bound.signature_hash);
}

#[test]
fn overloads_are_part_of_the_implementations_signature() {
    let one = ts("export function f(a: string): void;\nexport function f(a: any) {}\n");
    assert_eq!(one.items.len(), 1, "{:#?}", one.items);
    assert_eq!(one.items[0].lines, (1, 2));
    let two = ts("export function f(a: number): void;\nexport function f(a: any) {}\n");
    assert_ne!(one.items[0].signature_hash, two.items[0].signature_hash);
    let methods = ts("class C { m(a: string): void;\n m(a: any) {} }");
    assert_eq!(methods.items.len(), 2, "{:#?}", methods.items);
    // In a declaration file each signature is an item of its own.
    let ambient = ts("declare function d(): void;\ndeclare class Q { m(): void; }");
    let names: Vec<&str> = ambient.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["d", "Q", "m"]);
    assert!(
        ambient
            .items
            .iter()
            .all(|i| i.visibility == Some(Visibility::Public))
    );
}

#[test]
fn default_exports_are_named_default() {
    let f = ts("export default function () {}\n");
    assert_eq!(f.items[0].name, "default");
    let c = ts("export default class { m() {} }\n");
    let names: Vec<&str> = c.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["default", "m"]);
}

#[test]
fn javascript_and_jsx_are_read_by_the_tsx_analyser() {
    assert!(Tsx.handles(&path("src/a.jsx")));
    assert!(Tsx.handles(&path("src/a.mjs")));
    assert!(!Tsx.handles(&path("src/a.ts")));
    assert!(TypeScript.handles(&path("src/a.d.ts")));
    assert!(!TypeScript.handles(&path("src/ts")));
    let f = Tsx.analyse(
        &path("a.jsx"),
        b"export function App() { return <div onClick={() => go()}>hi</div>; }\nclass Old { render() {} }\n",
    );
    assert!(f.clean, "{f:#?}");
    assert_eq!(by_name(&f, "App").calls.len(), 1);
    assert_eq!(by_name(&f, "Old").visibility, Some(Visibility::Private));
    assert_ne!(TypeScript.name(), Tsx.name());
}

#[test]
fn a_damaged_file_keeps_what_could_be_read() {
    let f = ts("export function good() {}\nfunction broken( {\nexport class After {}\n");
    assert!(!f.clean);
    assert!(f.items.iter().any(|i| i.name == "good"));
}

#[test]
fn hostile_input_never_panics() {
    let deep = "namespace a {".repeat(20_000);
    assert!(!ts(&deep).clean);
    let nested = format!(
        "{}function f() {{}}{}",
        "namespace a { ".repeat(100),
        "}".repeat(100)
    );
    let f = ts(&nested);
    assert!(f.items.is_empty(), "namespaces past the limit are not read");
    let _ = ts(&"(".repeat(50_000));
    let _ = ts(&"x => ".repeat(20_000));
    let _ = TypeScript.analyse(&path("a.ts"), &[0xff, 0xfe, 0x00, b'{', b'}']);
    let tokens = [
        "function",
        "class",
        "namespace",
        "export",
        "import",
        "from",
        "{",
        "}",
        "(",
        ")",
        "<",
        ">",
        "=>",
        "=",
        "x",
        ";",
        "extends",
        "\"",
        "'",
        "`",
        "/*",
        "*/",
        "\n",
        "@",
        "require",
        ".",
        ",",
        "private",
        "#a",
    ];
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    for round in 0..300 {
        let mut s = String::new();
        for _ in 0..200 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let i = usize::try_from(state % tokens.len() as u64).unwrap_or(0);
            s.push_str(tokens[i]);
            s.push(' ');
        }
        let analyser: &dyn LanguageAnalyser = if round % 2 == 0 { &TypeScript } else { &Tsx };
        let _ = analyser.analyse(&path("a.ts"), s.as_bytes());
        let _ = analyser.highlight(s.as_bytes());
    }
}

#[test]
fn export_declare_items_are_read_as_public() {
    let f = ts("export declare function f(): void;\nexport declare class K { m(): void }\n");
    let names: Vec<&str> = f.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["f", "K", "m"]);
    assert!(
        f.items
            .iter()
            .all(|i| i.visibility == Some(Visibility::Public))
    );
}

#[test]
fn a_methods_decorators_are_its_surface() {
    let a = ts("export class C { @Get('/a') m() {} }");
    let b = ts("export class C { @Get('/b') m() {} }");
    assert_ne!(
        by_name(&a, "m").signature_hash,
        by_name(&b, "m").signature_hash
    );
    assert_eq!(
        by_name(&a, "C").signature_hash,
        by_name(&b, "C").signature_hash
    );
    assert_eq!(by_name(&a, "m").lines, (1, 1));
}

#[test]
fn names_that_are_not_imports_or_items_give_no_invented_call() {
    let p = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    // A re-export binds no name here.
    let f = ts(
        "export { a as b } from './x';\nexport * as ns from './y';\nfunction b() {}\nexport function k() { b(); ns.q(); }\n",
    );
    assert_eq!(by_name(&f, "k").calls, vec![p(&["b"])]);
    // A `require` in a function binds a name in that function only.
    let f = ts("function a() { const db = require('./a'); }\nexport function k() { db.x(); }\n");
    assert!(
        by_name(&f, "k").calls.is_empty(),
        "{:?}",
        by_name(&f, "k").calls
    );
    // Loop variables and a function expression's own name are local.
    let f = ts("function g() {}\nexport function k(fs) { for (const g of fs) g(); }\n");
    assert!(by_name(&f, "k").calls.is_empty());
    let f = ts("function h() {}\nexport const k = function h() { h(); };\n");
    assert!(by_name(&f, "k").calls.is_empty());
    // `import { "a-b" as c }`: the name without its quotes.
    let f = ts("import { \"a-b\" as c } from './x';\nexport function k() { c(); }\n");
    assert_eq!(by_name(&f, "k").calls, vec![p(&["./x", "a-b"])]);
}

#[test]
fn renaming_an_overloaded_function_keeps_its_hashes() {
    let a = ts("export function f(a: string): void;\nexport function f(a: any) {}\n");
    let b = ts("export function g(a: string): void;\nexport function g(a: any) {}\n");
    assert_eq!(a.items[0].signature_hash, b.items[0].signature_hash);
}

#[test]
fn names_exported_later_and_default_arrows_are_public_items() {
    let f = ts(
        "const f = () => 1;\nfunction g() {}\nfunction h() {}\nexport { f };\nexport default g;\n",
    );
    assert_eq!(by_name(&f, "f").visibility, Some(Visibility::Public));
    assert_eq!(by_name(&f, "g").visibility, Some(Visibility::Public));
    assert_eq!(by_name(&f, "h").visibility, Some(Visibility::Private));
    let d = ts("export default async () => { go(); };\n");
    assert_eq!(d.items.len(), 1);
    assert_eq!(d.items[0].name, "default");
}

#[test]
fn large_and_deep_inputs_take_linear_time() {
    use std::time::Instant;
    let deep = format!("function f() {{ {}", "{ a; ".repeat(50_000));
    let methods = format!("class C {{ {} }}", "m() {} ".repeat(30_000));
    for src in [deep, methods] {
        let start = Instant::now();
        let _ = ts(&src);
        let _ = TypeScript.highlight(src.as_bytes());
        // Well over the time a linear pass takes in a debug build, and
        // well under what the quadratic one took (80 s for 0.5 MB).
        assert!(start.elapsed().as_secs() < 20, "{:?}", start.elapsed());
    }
}
