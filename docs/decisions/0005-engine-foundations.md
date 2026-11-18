# 0005: Engine foundations — git access, query engine, index storage and location

**Status:** accepted, 2026-11-16; adding `gix` waits on the licence answer in #33 (see below). **Issue:** #22. The prototypes were not committed; their code and API notes are in the comments on #22.

## Context

M1 builds the L1–L2 model at a commit (REQUIREMENTS §3.3), persists it, and must meet these budgets:

| Operation | Budget |
|---|---|
| L1–L2 on screen, medium repository | 1 s |
| L1–L2 on screen, large repository | 3 s |
| Reopen a previously indexed repository | 200 ms |
| Re-index after a one-file change | 50 ms |

§5.2 names `gix`, `salsa` (or equivalent) and "an embedded, memory-mapped, content-addressed index".

## Decisions and measurements

All measurements: 4-CPU Linux container, release builds, the pinned reference repositories (DR 0001).

**1. Read code at commits, through the git object database, with `gix` 0.90.** Never the working tree; that waits for M6. Never the `git` binary (§5.2). Git blob and tree ids serve as the content hashes, so "no file is parsed twice" (§4) comes from the ids themselves.

| | Walk tree at `HEAD` | Read all blobs, single thread |
|---|---|---|
| medium | 1.1–1.6 ms | 48–64 ms (22 MB) |
| large | 53 ms warm, 377 ms cold | 0.63–0.80 s (230 MB) |

**Licence caveat.** `gix` is MIT OR Apache-2.0, and so are almost all of the crates it reaches. The exceptions:
- `foldhash` and `zlib-rs` (Zlib) and `uluru` (MPL-2.0) are on §8.1's list.
- `unicode-normalization` (via `gix-utils`, not optional) is `(MIT OR Apache-2.0) AND Unicode-3.0`, which is **not** on the list.

That is the same licence question already open in #33. Adding `gix` to the workspace therefore waits for the owner's answer there. If the answer is no, the fallback is to read loose and packed objects with `gix-object`, `gix-pack` and `gix-odb` directly, if they avoid `gix-utils`, or else with a minimal reader of our own.

**2. Incremental queries with `salsa` 0.28** (Apache-2.0 OR MIT), measured on 64,465 file inputs, the large repository's blob count:

| Step | Time |
|---|---|
| Create the inputs | 94–115 ms |
| Cold roll-up over all files | about 130 ms |
| Unchanged re-query | < 1 µs |
| Re-query after one file changes | **10.8–11.3 ms** |

Roll-ups will be hierarchical (per module, then per system) to keep that number down.

**3. Index storage: immutable, content-addressed segment files, read with `memmap2`.** Each segment is a sorted key index, then offsets, then values, and ends with a checksum. A re-index writes a new small segment; segments are merged later.

| Store | Write 64k records | Open | 64k point reads | File |
|---|---|---|---|---|
| `redb` 4.6 | 169–198 ms | 0.5 ms | 44–50 ms | 18.5 MB |
| Flat segment | 29–43 ms | 0.03 ms | 14–19 ms | 13 MB |

`redb` no longer memory-maps its file, which §5.2 asks for, and it is 3–4× slower here. It remains the fallback if transactional updates turn out to be needed.

`Mmap::map` is the single `unsafe` call. It lives in its own small `store` crate whose README justifies it (§5.3):
- the files belong to Blueprint, never to the repository under review;
- a checksum mismatch, a truncated file or a version mismatch means the index is rebuilt, never trusted (§5.5);
- the reader is fuzzed (#28).

**4. Index location: the platform cache directory**, via the `directories` crate (MIT OR Apache-2.0):

| Platform | Location |
|---|---|
| Linux | `~/.cache/blueprint/index/<key>` |
| macOS | `~/Library/Caches/blueprint/index/<key>` |
| Windows | `%LOCALAPPDATA%\blueprint\cache\index\<key>` |

- `<key>` is a hash of the repository's canonical absolute path. The index also stores that path, and the ids of the repository's root commits, and refuses to use itself if either does not match.
- Nothing is ever written inside the repository under review, so reviewing it leaves it untouched (§5.5).
- A cache can be deleted at any time, and the index can always be rebuilt (§5.5).

## Revisit when

- Re-indexing one file in the real engine exceeds 50 ms on the large repository: revisit salsa granularity before the storage choice.
- Segment merging proves more complex than a transactional store would be: switch to `redb` with a new decision record.
- A future `gix` upgrade changes the object-access API (it changed between versions during this research).
