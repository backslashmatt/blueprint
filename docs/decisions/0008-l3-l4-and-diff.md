# 0008: L3–L4 analysis, node identity, incremental re-index and the architectural diff

**Status:** accepted (agent decision, 2026-11-18). **Issue:** #60.

## Context

M2 adds L3 (types, and file-level groups of functions) and L4 (functions and methods) for Rust, re-indexing after one-file changes, and `bp diff` (§3.3, §6). The same choices will apply to every later language (§3.4). Each decision below is one a future session might question.

## Decisions

**1. Plug-in boundary.**
- A `LanguageAnalyser` trait lives in `model`, which has no I/O. Each language is a `lang-*` crate (§5.3), starting with `lang-rust`.
- The trait sees **one file at a time**, its repository path and its bytes. It returns `FileFacts`: the file's L3/L4 items, with line spans, visibility, a signature hash and a body hash, plus its **unresolved references**: `use` paths, `impl Trait for Type`, and call names.
- Resolving references into edges is a separate, cross-file engine pass.
- *Rejected: analysers that see the whole tree.* Per-file facts are what make per-file caching and parallel parsing possible.

**2. Parsing: tree-sitter (`tree-sitter` 0.25 and `tree-sitter-rust` 0.24, MIT).**
- §3.4 requires tree-sitter grammars, and their error recovery keeps a broken file partly analysed (§5.5). `syn` rejects a whole file on any error.
- Files are parsed in parallel with `rayon` (MIT OR Apache-2.0).
- Measured on 4 CPUs (fat LTO, best run). medium: 1,510 files, 17 MB, 0.97 s on 2 threads. large: 39,682 files, 147 MB, 11.6 s on 2 threads, 5.3 s on 4. Both are inside the "first index" budgets of 10 s and 120 s.
- Cost: about 1.7 MB of binary and 14 s of clean build.
- tree-sitter flags errors in 19% of medium's files and 9% of large's. These are mostly deliberately invalid test fixtures. Such files keep what was recovered and are marked `inferred`, never dropped.

**3. Node identity: by location, as for L1–L2.**
- An L3 id is its L2 module's path plus the item name, for example `L3:cargo:crates/engine#l1/cargo/Manifest`. Free functions in a module group under the reserved segment `(functions)`.
- An L4 id is its L3 path plus the function name.
- Methods attach to their type's L3 when the type is in the same crate. Otherwise they attach to an `impl <Type>` component in the impl's module.
- A trait method's segment is `<Trait>::<name>`. A clash left after that, such as `new` in two `impl Foo<T>` blocks, gets `~2`, `~3` and so on in source order.
- Content is never part of an id. *Moved* and *renamed* are therefore worked out by the diff (decision 6), not encoded in ids.

**4. Surface and change kind.**
- The **signature hash** covers an item's tokens outside its body: visibility, generics, parameters, return type, fields and variants. Whitespace and comments are ignored.
- The **body hash** covers the body's tokens.
- For a public item (`pub`, or `pub(crate)` or narrower, as recorded), a change of signature hash or visibility is a *surface change*.
- A change only to the body hash, with no edge added or removed, is *contained*.

**5. Incremental re-index.**
- `FileFacts` are stored in the index (#58), keyed by **blob id + analyser version**. Indexing a commit parses only the blobs that are new to the store.
- Resolution then runs over all facts. To meet the 50 ms one-file budget on large, the engine keeps the resolved model of the previous commit, plus a name index. It re-resolves only the changed file's own references, and the references elsewhere that name items the change added or removed.
- **Budget scope (interpretation):** the §4 row "Re-index after a one-file change — 50 ms" is measured **in process, with the engine resident**, as the app and the M6 IDE will run it. A fresh `bp` process on large spends about 120 ms just loading the L1–L2 model (`open_indexed`), so that cannot be the intended scope.
- bp-bench measures `reindex_one_file` in process.
- `salsa` (DR 0005) is not needed for this explicit delta. Revisit if delta rules multiply.

**6. Diff.**
- The two models are compared by id. Edges are compared by `(from, to, kind)`.
- Removed and added nodes that are left unmatched are paired when they have the same kind and the same body hash. They are reported as **moved** if the parent differs, and as **renamed** if the parent is the same and the name differs. Any other pair stays removed + added.
- **Findings:**
  - a *new cross-boundary dependency*: an L1 or L2 edge, between different nodes, that exists only in head;
  - a *new cycle*: a strongly connected component of size > 1 in head's L1 or L2 `depends-on` graph that contains an edge new in head.
- **Change weight:** each L4 node gets its changed lines, from a line diff of its span using `imara-diff` (Apache-2.0), split into signature and body lines. The weight is summed up the levels.
- **Files with no analyser** are an L3 component with `unknown` confidence. Their weight comes from a plain line diff, and their L4 view is that text diff (§3.4).
- The output is deterministic JSON, documented in `docs/diff-json.md`, with text output for people.

## Revisit when

- `reindex_one_file` misses 50 ms with the explicit delta: adopt `salsa`.
- Body-hash matching misses moves where a function was edited as it moved, and the fixture suite (#65) shows it: add a similarity score.
- A language needs whole-program information that per-file facts can't carry.
