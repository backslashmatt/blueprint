# blueprint-store

The persisted index (DR 0005): immutable, checksummed segment files mapping
32-byte content keys to values, read through a memory map. The format is
described at the top of `src/segment.rs`.

A segment that is corrupt, truncated or from another format version is never
an error. `Store::open` deletes it and reports it, and its entries are analysed
again (REQUIREMENTS §5.5).

## Why this crate uses `unsafe`

There is exactly one `unsafe` call: `memmap2::Mmap::map` in `Segment::open`.
REQUIREMENTS §5.2 asks for a memory-mapped index. Mapping means that reopening
a repository costs only validation, with no read or deserialisation, which
serves the 200 ms reopen budget.

Mapping a file is `unsafe` because the program's memory then changes if anyone
modifies the file. That is undefined behaviour while a slice of it is borrowed.
The crate keeps this from happening as follows:

- Segment files belong to Blueprint. They live in its cache directory, never
  in the repository under review.
- A segment is written under a temporary name, synced, and renamed into place.
  It is never opened for writing again: a change always makes a new file,
  named by its checksum.
- Before any lookup, every header field, offset and key is validated and the
  checksum is checked against the whole file. After that, reads cannot go out
  of bounds.
- Old segments are unmapped before they are deleted. Deleting a mapped file is
  harmless on Unix and refused on Windows.

The remaining risk is another program truncating or rewriting a segment while
Blueprint has it open. On Unix that would end the process with `SIGBUS`. The
user's own tools have no reason to touch these files. If this is ever seen in
practice, `Segment::from_bytes`, which reads the file into memory, is the
drop-in fallback.

The xtask layering test allows `#[allow(unsafe_code)]` only in crates listed in
`UNSAFE_CRATES` whose README has this section. Every other crate inherits
`unsafe_code = "deny"`.

## Fuzzing

`fuzz/` holds a `cargo-fuzz` target for the segment reader. It needs nightly
Rust:

```sh
cd crates/store/fuzz && cargo +nightly fuzz run segment
```

On stable, `tests/store.rs` runs 20,000 deterministic mutations of valid
segments on every CI run, along with exhaustive truncation and bit-flip tests.
