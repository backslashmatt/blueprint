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
- A segment is written under a unique temporary name, synced, and renamed into
  place. It is never opened for writing again: a change always makes a new
  file, named by its checksum.
- Before any lookup, every header field, offset and key is validated and the
  checksum is checked against the whole file. After that, reads cannot go out
  of bounds.
- `compact` opens the merged segment before unmapping and deleting the old
  ones. Deleting a mapped file is harmless on Unix and refused on Windows.

The remaining risk is another program truncating or rewriting a segment while
Blueprint has it open. On Unix that would end the process with `SIGBUS`. The
user's own tools have no reason to touch these files. If this is ever seen in
practice, `Segment::from_bytes`, which reads the file into memory, is the
drop-in fallback.

Every other crate inherits `unsafe_code = "forbid"`, so rustc itself rejects
`unsafe` there. This crate declares its own copy of the workspace lints with
`unsafe_code = "deny"`, so that the one call can be allowed. The xtask layering
test checks that:
- the copy matches the workspace lints apart from that one line;
- the crate is listed in `UNSAFE_CRATES` and its README has this section;
- `src/` contains exactly one `unsafe` block.

## Files

A segment is named `v<format version>-<checksum>.seg`.
- Identical content gets the same name.
- `Store::open` deletes segments named for an older format version.
- It leaves a newer version's segments alone, so two Blueprint versions
  sharing a cache do not keep deleting each other's index.
- Temporary files are deleted only once they are an hour old, so a write in
  progress in another process is never disturbed.

## Fuzzing

`tests/store.rs` runs 20,000 deterministic mutations of valid segments on
every CI run, along with exhaustive truncation and bit-flip tests. A
`cargo-fuzz` target for the reader is in its own pull request, because
`libfuzzer-sys` carries the NCSA licence, which needs the owner (#33).
