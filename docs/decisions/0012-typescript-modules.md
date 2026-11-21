# 0012: TypeScript and JavaScript in the model

**Status:** accepted (agent decision, 2026-11-21). **Issues:** #136, #138, #139.

## Context

M5 adds TypeScript and JavaScript, the second language of §3.4. DR 0008 fixed the plug-in boundary: per-file facts from a `LanguageAnalyser`, resolved into edges by the engine. Rust's L2 modules come from its module tree. TypeScript has no module declarations, so L2 needs its own rule.

## Decisions

**1. One crate, two analysers (`lang-ts`, tree-sitter-typescript 0.23, MIT).**
- `ts` reads `.ts`, `.mts` and `.cts`. `tsx` reads `.tsx` and JavaScript (`.js`, `.jsx`, `.mjs`, `.cjs`), since the TSX grammar reads JavaScript as a superset.
- The two grammars read some bytes differently (`<T>x` is a cast in one, JSX in the other). Facts must depend on the bytes alone (DR 0008), so each grammar is its own analyser with its own name in the facts key.
- *Rejected:* tree-sitter-javascript for `.js`. It is one more grammar to build and ship for little gain: the TSX grammar parses JSX and modern JavaScript.

**2. L2: each file of a package is a module.**
- An npm or Nx system's `.ts`, `.tsx`, `.js`… files are each an L2 module, named by their path in the package without the extension (`src/util/x`). An `index.*` file is its directory's module (`src/util`), as `mod.rs` is in Rust; the package's own `index.*` is the system itself.
- This matches what an ES module is (a file), and what a Rust file module is, so L3 "file-level group of functions" means the same in both languages.
- When two files would be one module (`x.ts` and `x.js`, or `x.ts` and `x/index.ts`), the first in path order wins and the rest are shown by directory, `unknown`. Files under `node_modules` are vendored code and are shown by directory.
- Module confidence is `exact` (the file exists) and visibility `public`: whether a file is reachable through the package's `exports` is not read yet.
- *Rejected: a directory as the module.* Free functions of different files would merge into one `(functions)` component, losing the file, which is how TypeScript code is organised and imported.

**3. Items and surface.** As DR 0008 decision 4, with TypeScript's rules:
- types: `class`, `interface`, `enum`, `type`. A class's fields and heritage are its signature and its methods its body (as a Rust trait's); interfaces, enums and type aliases are all signature.
- functions: declarations and module-level `const f = () => …`; overload signatures are part of the implementation's signature.
- visibility: `export` (or `declare`) is public; members are public unless `private`/`#x` or `protected` (restricted).

**4. References keep the specifier whole.** `import { a as b } from "./x"` is a `Use` reference with path `["./x", "a"]` and alias `b`. Resolving `./x` against the file's directory and the package's dependencies is the engine's job (#139). Until then, TypeScript files give no edges at all: a missing edge is better than an invented one (§2.3).

**5. Index keys.** Facts are keyed by analyser name and version as before; the L1–L4 model key now holds one number for all analysers' names and versions, and `ANALYSER_VERSION` went to 3 because npm packages' L2 changed.

## Measured

- Build: the grammar's C code compiles in 2.3 s (debug, 4 cores); its static archive is 3.1 MB. Installed size is measured in the PR that links it into `bp` and the app (#138).
- The reference repositories are all Rust, so their benchmarks parse no more files.

## Revisit when

- `tsconfig.json` `paths` or package `exports` are needed to place or resolve modules correctly.
- A repository has many `x.ts` + `x.js` pairs (compiled output committed): prefer the `.ts`.
