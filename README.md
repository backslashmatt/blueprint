# Blueprint

A native code review app that presents a change as a change to the system's architecture, which the reviewer drills into until they reach code.

The product definition, performance budgets and working rules are in [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md).

## Layout

```
crates/
  model/      Types for nodes, edges, levels, diffs. No I/O, no workspace dependencies.
  engine/     Indexing, incremental queries, git, storage, diffing. Headless.
  protocol/   The serialisable API between the engine and any front end.
  cli/        Headless front end; the `bp` binary.
  ui/         View state and rendering. Depends on `protocol` only.
  app/        Platform shell: windows, menus, packaging.
bench/        Benchmarks for the budgets in REQUIREMENTS §4.
xtask/        Repository tooling. Its tests enforce the crate layering rules.
docs/decisions/  Decision records.
```

## Building and checking

The toolchain is pinned in `rust-toolchain.toml`.

```
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

`unsafe` is forbidden, and `unwrap`, `expect` and `panic!` are denied outside tests (workspace lints in `Cargo.toml`).
