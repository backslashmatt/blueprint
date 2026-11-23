# 0015: Go in the model

**Status:** accepted (agent decision, 2026-11-23). **Issues:** #160 (analyser), #164 (engine), and the edges issue that follows.

## Context

`lang-go` (#161) reads one file at a time. It gives:
- types;
- functions;
- methods, each with its receiver's type (`func (s *Store) Get()` is a method of `Store`);
- imports, embedding and calls.

L1 already makes one system per `go.mod`. L2 needs a rule.

In Go the unit of code is the **package**: a directory whose files share one namespace. A type is often declared in one file and its methods in others. Every exported name of the package is reachable from any of its files and through any import of it.

## Options

1. **Package (directory) is the module (chosen).** This is how Go code is imported (`import "example.com/app/store"`), named and reviewed, and it matches §3.1, "a package … with a public surface".
2. **File is the module.** This is the rule for TypeScript and Python. But a Go file is not importable on its own, and it would split a type from its methods.

## Decision

**L2 modules:**
- Each directory of a Go module that holds `.go` files is an L2 module, named by its path in the module (`internal/store`). The module root's package is the system itself.
- `_test.go` files, an external `x_test` package included, belong to their directory's module.
- Directories and files the go tool ignores (`testdata`, `vendor`, `_x`, `.x`, and files named `_x.go` or `.x.go`, such as editor lock files) are shown by directory, as L1 already ignores them. Otherwise a stray `.#main.go` would take `main`'s id and turn the real one into `main~2`.
- A `.go` file belongs to the nearest enclosing `go.mod`, whatever other manifests sit between. A `ui/package.json` beside `ui/embed.go` does not take the Go package away from its module. Other files keep the general rule: the deepest system, then the one whose language they are.
- A package under an `internal` directory is restricted (only its parent's tree may import it); any other is public.

**L3–L4:**
- Every file's items are placed in its package.
- Types are added before functions, so a method attaches to its type whichever file declares it (`L4:go:#store/Store/Get`).
- A method whose type the package does not declare goes to an `impl T` group, as for Rust.
- A package's free functions form one `(functions)` component.

**Incremental:**
- A body-only edit applies in place.
- Any change to a Go file's items rebuilds, because a package's files share nodes: a method's type lives in another file, and so does the `(functions)` group's source. C# partial types are handled the same way (DR 0013).
- A rebuild is correct but costs more than a by-item update. A finer rule can come later if Go reference repositories show the 50 ms re-index budget is at risk.

**Index keys:** `ANALYSER_VERSION` goes to 7, because Go systems now have L2–L4 nodes.

**Edges (#166, `l34/edges/go.rs`).**
- An import path resolves against the module path (the L1 node's name) of the importing module and of the local modules it requires (its L1 edges). The longest module path that prefixes the import wins, and the rest is the package, which must exist. The standard library and external modules give no edge.
- **depends-on (L2):** each import, `exact`.
- **extends (L3):** each embedded type, `Base` in the package or `pkg.T` through an import.
- **calls (L4):**
  - `F()` in the package, or in the one dot import that has it;
  - `pkg.F()` through an import;
  - `T.M()` and `pkg.T.M()`, which are method expressions;
  - `self.m()` to the receiver's type, then to the one embedded type (declared in the same file) that has the method.
  - Calls through values need types and give none.
- **Names:**
  - An import binds its alias, or its package's `package` clause name. `lang-go` records the clause in `FileFacts::package`, a new optional field, so `FACTS_FORMAT` goes to 2. If a package's files disagree, or none says, nothing is bound: guessing from the directory once bound a name the file used for a variable.
  - An import from outside the repository still claims the name the go tool would give it, so a local package named alike is not bound in that file (stdlib `log` beside a local `log`). Any name two imports claim is not bound.
  - Only exported names come through an import or a dot import, so `min` stays the builtin.
  - An import named `self` turns off `self.m()` resolution in its file.
- **Confidence:** an import into another module is as sure as the L1 dependency it goes through (`inferred` for a `require` matched only by name). Within a module it is `exact`.
- **Known imprecision:** a field of function type shadows a promoted method of the same name, which the facts cannot see, so such a `self.m()` still resolves to the method. Calls are `inferred`.
- `ANALYSER_VERSION` goes to 8 with edges.

## Measured

- The reference repositories are all Rust, so the benchmarks see no Go files. The CI bench rows of the PR that adds this record show any cost of the extra dispatch.

## Revisit when

- Go repositories enter the benchmarks and item edits there exceed the re-index budget: update by item, with methods tracked across files.
- Build tags (`//go:build`) need to split a package's files by platform.
