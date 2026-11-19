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
bench/        Benchmarks for the budgets in REQUIREMENTS §4 (see docs/benchmarks.md).
xtask/        Repository tooling. Its tests enforce the crate layering rules.
docs/            User documentation (`cli.md`) and decision records (`decisions/`).
```

## Building and checking

The toolchain is pinned in `rust-toolchain.toml`.

```
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

`unsafe` is forbidden. `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!` and `dbg!` are denied (workspace lints in `Cargo.toml`); `clippy.toml` allows the first three in tests, and an xtask test fails if any source outside `tests/` switches one of these lints off.

## Licence

Blueprint is licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT licence](LICENSE-MIT), at your option. Unless you state otherwise, any contribution you intentionally submit for inclusion in Blueprint, as defined in the Apache-2.0 licence, is dual licensed as above, without any additional terms or conditions.
