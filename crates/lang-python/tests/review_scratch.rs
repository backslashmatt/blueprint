use blueprint_lang_python::analyse;
use blueprint_model::lang::{FileFacts, Item};

fn py(s: &str) -> FileFacts { analyse(s.as_bytes()) }
fn names(s: &str) -> Vec<String> { py(s).items.iter().map(|i| i.name.clone()).collect() }
fn it(s: &str, n: &str) -> Item { py(s).items.into_iter().find(|i| i.name == n).unwrap_or_else(|| panic!("no {n} in {s}")) }
fn calls(s: &str, n: &str) -> Vec<String> { it(s, n).calls.iter().map(|c| c.join(".")).collect() }

#[test] fn else_except_finally_defs() {
    let n = names("try:\n    import x\nexcept ImportError:\n    def f(): pass\nelse:\n    def g(): pass\nfinally:\n    def h(): pass\nif a:\n    pass\nelif b:\n    def i(): pass\nelse:\n    def j(): pass\n");
    assert_eq!(n, ["f","g","h","i","j"]);
}
#[test] fn string_escape_hash() {
    let a = it("def f():\n    return \"abc\\n\"\n", "f");
    let b = it("def f():\n    return \"xyz\\n\"\n", "f");
    assert_ne!(a.body_hash, b.body_hash);
}
#[test] fn string_escape_sig() {
    let a = it("def f(x=\"abc\\n\"):\n    pass\n", "f");
    let b = it("def f(x=\"xyz\\n\"):\n    pass\n", "f");
    assert_ne!(a.signature_hash, b.signature_hash);
}
#[test] fn fstring_hash() {
    let a = it("def f(x):\n    return f\"a{{b}}{x}\"\n", "f");
    let b = it("def f(x):\n    return f\"z{{b}}{x}\"\n", "f");
    assert_ne!(a.body_hash, b.body_hash);
}
#[test] fn class_comment_body() {
    let a = it("class A:\n    x: int\n", "A");
    let b = it("class A:\n    # note\n    x: int\n", "A");
    assert_eq!((a.body_hash, a.body_line, a.signature_hash), (b.body_hash, b.body_line, b.signature_hash));
}
#[test] fn nested_param_shadows() {
    assert_eq!(calls("def f():\n    g = lambda helper: 0\n    helper()\n", "f"), ["helper"]);
}
#[test] fn nested_def_param_shadows() {
    assert_eq!(calls("def f():\n    def g(helper):\n        pass\n    helper()\n", "f"), ["helper"]);
}
#[test] fn match_capture() {
    assert_eq!(calls("def f(x):\n    match x:\n        case [handler]:\n            handler()\n", "f"), Vec::<String>::new());
}
#[test] fn match_as() {
    assert_eq!(calls("def f(x):\n    match x:\n        case Foo() as h:\n            h.run()\n", "f"), ["Foo"]);
}
#[test] fn builtins_staticmethod() {
    assert_eq!(calls("class C:\n    @builtins.staticmethod\n    def m(x):\n        x.run()\n", "m"), Vec::<String>::new());
}
#[test] fn staticmethod_call() {
    assert_eq!(calls("class C:\n    @staticmethod()\n    def m(x):\n        x.run()\n", "m"), Vec::<String>::new());
}
#[test] fn typeparam() {
    assert_eq!(calls("def f[T](x):\n    return T()\n", "f"), Vec::<String>::new());
}
#[test] fn import_in_func_shadow_is_fine() {
    println!("{:?}", calls("def f():\n    from m import helper as h\n    h()\n", "f"));
}
#[test] fn del_and_nonlocal() {
    println!("{:?}", calls("def f():\n    x = 1\n    def g():\n        nonlocal x\n        x()\n", "f"));
}
#[test] fn self_reassigned() {
    println!("{:?}", calls("class C:\n    def m(self=None):\n        self.go()\n", "m"));
    println!("{:?}", calls("class C:\n    def m(self, /):\n        self.go()\n", "m"));
    println!("{:?}", calls("class C:\n    def m(*args):\n        args.go()\n", "m"));
}
#[test] fn star_params() {
    assert_eq!(calls("def f(*, cb, **kw):\n    cb()\n    kw.get()\n", "f"), Vec::<String>::new());
    assert_eq!(calls("def f(*cb: int):\n    cb.x()\n", "f"), Vec::<String>::new());
    assert_eq!(calls("def f(**cb: int):\n    cb.x()\n", "f"), Vec::<String>::new());
}
#[test] fn imports() {
    let f = py("from . import (a, b)\nfrom .x.y import (c as d,)\nfrom ... import e\nfrom ...z import *\nimport a.b.c as q\nfrom __future__ import annotations\nimport __future__\n");
    for r in &f.references { println!("{r:?}"); }
}
#[test] fn bases() {
    let f = py("class A(namedtuple('A','x'), *bs, B[int], metaclass=M, **kw):\n    pass\n");
    for r in &f.references { println!("{r:?}"); }
}
#[test] fn items_misc() {
    println!("{:?}", names("class A:\n    if X:\n        def m(self): pass\n    else:\n        def m(self): pass\n    try:\n        class B: pass\n    except E:\n        class B: pass\n"));
    println!("{:?}", names("match x:\n    case 1:\n        def f(): pass\n"));
    println!("{:?}", names("class A:\n    type T = int\n"));
    let f = py("@overload\ndef f(x: int) -> int: ...\n@overload\ndef f(x: str) -> str: ...\ndef f(x): return x\n");
    for i in &f.items { println!("{} {:?} {:?}", i.name, i.lines, i.body_hash); }
    let f = py("@dec\n\n# c\n@dec2\ndef f():\n    pass\n");
    for i in &f.items { println!("{} {:?} {:?}", i.name, i.lines, i.body_line); }
}
#[test] fn sig_name_alias() {
    let a = it("type A = int\n", "A"); let b = it("type B = int\n", "B");
    println!("alias rename sig same? {}", a.signature_hash == b.signature_hash);
    let a = it("class A: pass\n", "A"); let b = it("class B: pass\n", "B");
    println!("class rename sig same? {}", a.signature_hash == b.signature_hash);
}
#[test] fn sep() {
    let a = it("def f(a, b):\n    pass\n", "f");
    let b = it("def f(a, *, b):\n    pass\n", "f");
    let c = it("def f(a, /, b):\n    pass\n", "f");
    assert_ne!(a.signature_hash, b.signature_hash);
    assert_ne!(a.signature_hash, c.signature_hash);
}
#[test] fn chained_class_attr() {
    let a = it("class A:\n    x = y = 1\n", "A");
    let b = it("class A:\n    x = z = 1\n", "A");
    println!("chained attr sig equal: {}", a.signature_hash == b.signature_hash);
}
#[test] fn docstring_reindent() {
    let a = it("def f():\n    \"\"\"a\n    b\"\"\"\n", "f");
    let b = it("def f():\n        \"\"\"a\n        b\"\"\"\n", "f");
    println!("docstring reindent body equal: {}", a.body_hash == b.body_hash);
}
#[test] fn nested_def_params_invent() {
    assert_eq!(calls("def f():\n    def g(cb):\n        cb()\n    return g\n", "f"), Vec::<String>::new());
}
#[test] fn class_scope_in_func() {
    println!("cls {:?}", calls("def f():\n    class K:\n        def m(self):\n            self.go()\n", "f"));
}
#[test] fn lambda_in_class_default() {
    println!("comp {:?}", calls("def f(xs):\n    return {k: v() for k, v in xs}\n", "f"));
    println!("except* {:?}", calls("def f():\n    try:\n        pass\n    except* E as eg:\n        eg.split()\n", "f"));
    println!("import local {:?}", calls("def f():\n    import a.b\n    a.b.c()\n", "f"));
    println!("for else {:?}", names("for x in y:\n    pass\nelse:\n    def z(): pass\nwhile a:\n    pass\nelse:\n    def w(): pass\n"));
}
#[test] fn hl_perf() {
    use blueprint_lang_python::highlight::highlight;
    for n in [2000usize, 4000, 8000] {
        let src = "x = ".to_owned() + &"[".repeat(n) + &"a, ".repeat(n) + &"]".repeat(n) + "\n";
        let t = std::time::Instant::now();
        let h = highlight(src.as_bytes());
        let t1 = t.elapsed();
        let t = std::time::Instant::now();
        let _ = analyse(src.as_bytes());
        println!("n={n} hl={t1:?} analyse={:?} toks={}", t.elapsed(), h.len());
    }
}
#[test] fn analyse_nested_class_perf() {
    for n in [16usize, 32, 63] {
        let mut s = String::new();
        for i in 0..n { s += &"    ".repeat(i); s += &format!("class A{i}:\n"); }
        let body = "    ".repeat(n);
        for j in 0..20000 { s += &body; s += &format!("x{j} = 1\n"); }
        let t = std::time::Instant::now();
        let f = analyse(s.as_bytes());
        println!("nest n={n} {:?} items={}", t.elapsed(), f.items.len());
    }
}
