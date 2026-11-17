# 0006: L2 reads Rust module declarations with a shallow lexer, not tree-sitter

**Status:** accepted, 2026-11-17. **Issues:** #27, #30. **Owner may overrule:** this is a departure from §5.2 ("Parsing: tree-sitter"), so it is flagged in the PR and the agent log.

## Context

The first L1–L2 benchmarks of the reference repositories (#30) used `bp model <checkout>`. L2 parsed every file reached by the module tree with `tree-sitter-rust`, to read only its `mod` declarations.

| | `bp model` total | of which L2 | §4 budget for L1–L2 on screen |
|---|---|---|---|
| medium (rust-analyzer) | 2.06 s | 1.95 s | 1 s |
| large (rust-lang/rust) | 15.8 s | most of it | 3 s |

These were measured locally on 4 CPUs; CI has 2. The cost is the full syntax tree of about 22 MB (medium) of Rust source, of which L2 needs only the item-level `mod` lines. Parallel parsing would not close the gap on a 2-CPU runner.

## Decision

- L2 reads `mod` declarations with a small Rust lexer (`engine::l2::lexer`). It skips comments (nested), strings, raw strings, byte and C strings, and character literals versus lifetimes, then matches brackets.
- Items are read from the tokens:
  - outer attributes (`#[path]`, `#[cfg_attr(.., path = ..)]`), visibility and `mod`;
  - inline module bodies;
  - `mod x;` inside macro calls such as `cfg_if!`.
- Every other body is skipped by its bracket match.
- A file with an unterminated comment or string, or unbalanced brackets, degrades its module to `unknown` (§5.5). The items before the damage are still read.
- tree-sitter stays the parser for syntax trees at L3–L4 (M2), where §5.2's incremental re-parse matters. It returns as a dependency then; until then it is removed, which also takes about 12 s off a clean build.

## Result

| | before | after |
|---|---|---|
| medium, `bp model` | 2.06 s | 0.15 s |
| large, `bp model` | 15.8 s | 1.2 s |
| warnings, medium / large | 9 / 316 | 0 / 16 |

The warnings fell because many were tree-sitter flagging recent Rust syntax as errors. The 16 that remain on the large repository are real: modules in git submodules that are not checked out, and `cfg`-selected paths.

## Revisit when

- L3 parses files with tree-sitter anyway: L2 could then take module items from those trees whenever they already exist, with the lexer kept for the first index.
- The lexer disagrees with rustc on a real repository: add the case to `lexer.rs`'s tests and fix it, or fall back to tree-sitter for that file.
