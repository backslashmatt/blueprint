# 0013: C# in the model

**Status:** accepted (agent decision, 2026-11-22). **Issues:** #144 (analyser), the engine issue that follows this record.

## Context

`lang-csharp` (#145) reads one file at a time: types, members, the namespace each item is declared in (`Item::module`), `using` directives and base lists. The engine has to place those items at L2. Rust and TypeScript place by file (a Rust module or an ES module *is* a file, DR 0008, DR 0012). C# does not: a namespace is declared in any number of files, one file may declare several namespaces, and a `partial` type is split across files. §3.1 gives L2 as "a package, **namespace** or directory with a public surface".

## Options

1. **Namespace is the module (chosen).** Each namespace declared in a project's files is an L2 node of that project's system (`App.Core` is the module `["App", "Core"]`); a type sits under the namespace it is declared in, whichever file declares it.
2. **File is the module**, as in TypeScript, with the namespace only a label. Simple for the engine, but every C# reviewer thinks in namespaces: `using App.Core;` names a namespace, not a file, so edges would have to be fanned out to every file of it, and the map would show a file structure C# code does not have.
3. **Directory is the module.** Usually mirrors namespaces by convention, but not reliably, and loses the namespace where it differs.

## Decision

- **L2:** within each `.csproj` system, one module per namespace any of its `.cs` files declares, nested by path (`App.Core.Io` is `["App", "Core", "Io"]`, under `App.Core` by its path, as Rust modules are). Code outside any namespace belongs to the system's root module. Files under `bin/` and `obj/` stay excluded (DR for L1 .NET).
- **Placement reads the file:** which namespaces a file declares is found by a lexical scan at L2 (comments and strings skipped), as Rust's `mod` items are, so the L1–L2 budget does not pay for a parse. That set is the file's L2 *shape* in the resident engine: an edit that changes it rebuilds; any other edit is applied by item or in place.
- **Many files per module:** a C# file's own module is its system's root; each item's namespace places it (as Rust's inline modules do). Because a namespace's nodes come from several files, the resident engine re-resolves by item only when the file's namespaces are unchanged, and the L2 node of a namespace exists while any file declares it.
- **Partial types are one L3 node:** the parts of `partial class K` in one namespace are one component `K`; its signature and body hashes combine the parts in path order, its span is the first part's, and members from every part sit under it. Until the engine merges them, a second part would appear as `K~2`, which misrepresents the design, so merging is part of the first engine slice.
- **Extends or implements:** C# writes both in a base list. The analyser reports bases neutrally; the engine gives `extends` when the base resolves to a `class` or `record`, `implements` when it resolves to an `interface`, and no edge when it does not resolve.
- **`using` and edges:** `using App.Core;` brings in a namespace's types; a reference to a type resolves through the file's usings and its own namespace and its parents, as C# does. External namespaces (`System.*`, NuGet packages) give no edge.

## Measured

Nothing yet: the engine slice will report the L1–L2 cost of the namespace scan on a C# repository and the bench rows on the (Rust) reference repositories.

## Revisit when

- Solution-wide `global using`s or `Directory.Build.props` matter for resolution (not read today, as L1 does not read them).
- A C# reference repository is added to the benchmarks and the per-namespace model is too slow to update by item.
