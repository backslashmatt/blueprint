//! The C# analyser's facts.

use blueprint_lang_csharp::{CSharp, analyse};
use blueprint_model::lang::{FileFacts, Item, ItemKind, LanguageAnalyser, Reference};
use blueprint_model::{RepoPath, Visibility};

fn cs(src: &str) -> FileFacts {
    analyse(src.as_bytes())
}

fn by_name<'a>(f: &'a FileFacts, name: &str) -> &'a Item {
    let found = f.items.iter().find(|i| i.name == name);
    found.unwrap_or_else(|| unreachable!("no item {name} in {:#?}", f.items))
}

fn item(src: &str, name: &str) -> Item {
    by_name(&cs(src), name).clone()
}

fn p(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| (*x).to_owned()).collect()
}

const SAMPLE: &str = r#"using System;
using B = Foo.Baz;
using static Foo.Util;
namespace App.Core;
[Serializable]
public partial class K<T> : Base<T>, IThing where T : class {
    private int x = 1;
    public string Name { get; set; }
    public K(int a) : base(a) { Helper(); Foo.Util.Go(); }
    public static void M(int a) { var y = new K<int>(1); y.Run(); a.Run(); Other(); }
    protected abstract int N(string s);
    public int this[int i] => i;
    class Nested { void Q() {} }
}
struct S { }
public interface IThing { void Run(); }
public enum E { A, B }
public record R(int X);
public delegate void D(int x);
"#;

#[test]
fn finds_types_and_members_in_source_order() {
    let f = cs(SAMPLE);
    assert!(f.clean, "{f:#?}");
    let ns = p(&["App", "Core"]);
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
    use Visibility::{Private, Public, Restricted};
    assert_eq!(
        got,
        vec![
            ("K", "partial class".into(), Some(Public)),
            ("Name", "method of K".into(), Some(Public)),
            ("K", "method of K".into(), Some(Public)),
            ("M", "method of K".into(), Some(Public)),
            ("N", "method of K".into(), Some(Restricted)),
            ("this[]", "method of K".into(), Some(Public)),
            ("K.Nested", "class".into(), Some(Private)),
            ("Q", "method of K.Nested".into(), Some(Private)),
            ("S", "struct".into(), Some(Restricted)),
            ("IThing", "interface".into(), Some(Public)),
            ("Run", "method of IThing".into(), Some(Public)),
            ("E", "enum".into(), Some(Public)),
            ("R", "record".into(), Some(Public)),
            ("D", "delegate".into(), Some(Public)),
        ]
    );
    assert!(f.items.iter().all(|i| i.module == ns));
    // Bodiless members and types have no body hash.
    assert_eq!(by_name(&f, "N").body_hash, None);
    assert_eq!(by_name(&f, "S").body_hash, None);
}

#[test]
fn usings_and_bases_are_references() {
    let f = cs(SAMPLE);
    let refs: Vec<String> = f
        .references
        .iter()
        .map(|r| match r {
            Reference::Use { path, alias, .. } => format!("use {} {alias:?}", path.join(".")),
            Reference::Impl {
                self_type, r#trait, ..
            } => format!("{} : {}", self_type.join("."), r#trait.join(".")),
        })
        .collect();
    assert_eq!(
        refs,
        [
            "use System.* None",
            "use Foo.Baz Some(\"B\")",
            "use Foo.Util.* None",
            "K : Base",
            "K : IThing",
        ]
    );
}

#[test]
fn calls_skip_locals_this_and_base() {
    let f = cs(SAMPLE);
    assert_eq!(
        by_name(&f, "K").calls.len(),
        0,
        "the class has no calls of its own"
    );
    let ctor = f
        .items
        .iter()
        .find(|i| i.name == "K" && matches!(i.kind, ItemKind::Method { .. }))
        .unwrap_or_else(|| unreachable!());
    assert_eq!(ctor.calls, vec![p(&["Helper"]), p(&["Foo", "Util", "Go"])]);
    // `y` is a local and `a` a parameter.
    assert_eq!(by_name(&f, "M").calls, vec![p(&["Other"])]);
    let lambda = item(
        "class C { void F(int[] xs) { xs.Select(x => x.Go()); foreach (var e in xs) e.Go(); Run(out var z); z.Go(); this.H(); base.H(); G<int>(); } }",
        "F",
    );
    assert_eq!(lambda.calls, vec![p(&["Run"]), p(&["G"])]);
}

#[test]
fn namespaces_nest_and_file_scoped_ones_apply_to_what_follows() {
    let f = cs("namespace A { namespace B.C { class X {} } class Y {} }\n");
    assert_eq!(by_name(&f, "X").module, p(&["A", "B", "C"]));
    assert_eq!(by_name(&f, "Y").module, p(&["A"]));
    let g = cs("class Before {}\nnamespace N;\nclass After {}\n");
    assert_eq!(by_name(&g, "Before").module, Vec::<String>::new());
    assert_eq!(by_name(&g, "After").module, p(&["N"]));
}

#[test]
fn body_and_signature_changes_are_told_apart() {
    let base = item(
        "public class C { public int F(int x) { return x + 1; } }",
        "F",
    );
    let ws = item(
        "public class C {\n  public int F( int x )\n  { // c\n    return x+1;\n  }\n}",
        "F",
    );
    assert_eq!(base.signature_hash, ws.signature_hash);
    assert_eq!(base.body_hash, ws.body_hash);
    let body = item(
        "public class C { public int F(int x) { return x + 2; } }",
        "F",
    );
    assert_eq!(base.signature_hash, body.signature_hash);
    assert_ne!(base.body_hash, body.body_hash);
    let sig = item(
        "public class C { public int F(long x) { return x + 1; } }",
        "F",
    );
    assert_ne!(base.signature_hash, sig.signature_hash);
    let renamed = item(
        "public class C { public int G(int x) { return x + 1; } }",
        "G",
    );
    assert_eq!(base.signature_hash, renamed.signature_hash);
    // A class: field types are surface; methods and initial values body.
    let c = item("public class C { int x = 1; void F() {} }", "C");
    let typed = item("public class C { long x = 1; void F() {} }", "C");
    let init = item("public class C { int x = 2; void F() {} }", "C");
    let method = item("public class C { int x = 1; void F() { G(); } }", "C");
    assert_ne!(c.signature_hash, typed.signature_hash);
    assert_eq!(c.signature_hash, init.signature_hash);
    assert_ne!(c.body_hash, init.body_hash);
    assert_eq!(c.signature_hash, method.signature_hash);
    let based = item("public class C : B { int x = 1; void F() {} }", "C");
    assert_ne!(c.signature_hash, based.signature_hash);
}

#[test]
fn only_cs_files_are_read() {
    let path = |s: &str| RepoPath::parse(s).unwrap_or_else(|_| unreachable!());
    assert!(CSharp.handles(&path("src/A.cs")));
    assert!(!CSharp.handles(&path("src/A.csproj")));
    assert!(!CSharp.handles(&path("src/a.ts")));
}

#[test]
fn a_damaged_file_keeps_what_could_be_read() {
    let f = cs("class Good { void F() {} }\nclass Broken { void G( { }\nclass After {}\n");
    assert!(!f.clean);
    assert!(f.items.iter().any(|i| i.name == "Good"));
}

#[test]
fn hostile_input_never_panics_and_stays_linear() {
    let deep = "namespace a {".repeat(20_000);
    assert!(!cs(&deep).clean);
    let nested = format!(
        "{}class X {{}}{}",
        "namespace a { ".repeat(100),
        "}".repeat(100)
    );
    assert!(cs(&nested).items.is_empty(), "past the depth limit");
    let start = std::time::Instant::now();
    let blocks = format!("class C {{ void F() {{ {} }} }}", "{ a(); ".repeat(50_000));
    let _ = cs(&blocks);
    let _ = CSharp.highlight(blocks.as_bytes());
    let members = format!("class C {{ {} }}", "void M() {} ".repeat(30_000));
    let _ = cs(&members);
    assert!(start.elapsed().as_secs() < 20, "{:?}", start.elapsed());
    let _ = analyse(&[0xff, 0xfe, 0x00, b'{', b'}']);
    let tokens = [
        "class",
        "namespace",
        "using",
        "public",
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
        ":",
        "\"",
        "'",
        "@\"",
        "$\"",
        "/*",
        "*/",
        "\n",
        "[",
        "]",
        ".",
        ",",
        "static",
        "this",
        "base",
        "get",
        "set",
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
        let _ = cs(&s);
        let _ = CSharp.highlight(s.as_bytes());
    }
}

#[test]
fn an_unclosed_type_at_the_top_keeps_what_could_be_read() {
    let f = cs("class A { void M() {}\nclass B { void N() {} }\n");
    assert!(!f.clean);
    let names: Vec<&str> = f.items.iter().map(|i| i.name.as_str()).collect();
    for want in ["M", "N"] {
        assert!(names.contains(&want), "{names:?}");
    }
}

#[test]
fn locals_of_every_form_give_no_call() {
    let src = r#"class C(ILogger log) {
  int P { get; set { value.Foo(); } }
  void M(Dictionary<int,int> d, int[] xs) {
    foreach (var (k, v) in d) k.Foo();
    var (a, b) = T(); a.Foo();
    if (d is var dv) dv.Foo();
    if (d is { Count: var cnt }) cnt.Foo();
    if (xs is [var first, ..]) first.Foo();
    var q = from x in xs let z = x select x.Foo() + z.Bar();
    var n = nameof(M);
    log.Info();
    Real();
  }
}"#;
    let f = cs(src);
    assert_eq!(by_name(&f, "M").calls, vec![p(&["T"]), p(&["Real"])]);
    assert!(
        by_name(&f, "P").calls.is_empty(),
        "{:?}",
        by_name(&f, "P").calls
    );
}

#[test]
fn a_records_base_arguments_are_not_part_of_its_base() {
    let f = cs("record R(int X) : Base(X), IFoo;");
    let bases: Vec<Vec<String>> = f
        .references
        .iter()
        .filter_map(|r| match r {
            Reference::Impl { r#trait, .. } => Some(r#trait.clone()),
            Reference::Use { .. } => None,
        })
        .collect();
    assert_eq!(bases, vec![p(&["Base"]), p(&["IFoo"])]);
}

#[test]
fn a_property_initial_value_is_body() {
    let a = item("class C { public int X { get; set; } = 5; }", "X");
    let b = item("class C { public int X { get; set; } = 6; }", "X");
    assert_eq!(a.signature_hash, b.signature_hash);
    assert_ne!(a.body_hash, b.body_hash);
}

#[test]
fn an_explicit_interface_implementation_names_its_interface() {
    let f = cs("class C : IFoo { void IFoo.Bar() {} void Bar() {} }");
    let traits: Vec<Option<String>> = f
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Method { r#trait, .. } => Some(r#trait.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(traits, vec![Some("IFoo".into()), None]);
}

#[test]
fn file_local_types_are_private_and_regions_change_nothing() {
    assert_eq!(
        item("file class F {}", "F").visibility,
        Some(Visibility::Private)
    );
    let plain = item("class C { int x; }", "C");
    let region = item("class C {\n#region R\n int x;\n#endregion\n}", "C");
    assert_eq!(plain.signature_hash, region.signature_hash);
}
