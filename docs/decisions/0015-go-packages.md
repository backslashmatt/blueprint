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
- Directories the go tool ignores (`testdata`, `vendor`, `_x`, `.x`) are shown by directory, as L1 already ignores them.
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

**Edges** (imports, embedding, calls) come in their own slice. Until then Go files give none (§2.3).

## Measured

- The reference repositories are all Rust, so the benchmarks see no Go files. The CI bench rows of the PR that adds this record show any cost of the extra dispatch.

## Revisit when

- Go repositories enter the benchmarks and item edits there exceed the re-index budget: update by item, with methods tracked across files.
- Build tags (`//go:build`) need to split a package's files by platform.
