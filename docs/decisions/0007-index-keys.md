# 0007: Key the index by tree id, and drop the repository identity check

**Status:** accepted (agent decision, 2026-11-17). It supersedes one point of DR 0005 §4.

## Context

DR 0005 put the index in the platform cache, under a hash of the repository's canonical path. It also said the index would store that path and the ids of the repository's root commits, and refuse to use itself if either did not match. That check guards against a cache that belongs to a different repository.

`bp index` (#28, #29) needs a key for what it stores. Finding a repository's root commits means walking its whole history. rust-lang/rust has hundreds of thousands of commits, so that walk alone would use up much of the 200 ms reopen budget.

## Options

1. **Key by commit id**, and keep DR 0005's identity check.
2. **Key by the commit's root tree id plus `ANALYSER_VERSION`**, with no identity check.

## Decision

**Option 2.**

- **Correct without the check.** A tree id is a hash of every file the analysers read. So a stored model is correct for any commit, in any repository, whose tree has that id. A cache that ends up shared, whether by a path-hash collision or by moving or replacing a repository, can only return a correct model.
- **Saves work across commits.** Commits with the same tree, such as a merge with no conflicts or a revert, share one entry.
- **Analyser changes:** `ANALYSER_VERSION` must be bumped whenever any analyser's output changes for the same files. Entries from older code then stop matching and are rebuilt.
- **Storage format:** the value is the model and its diagnostics, as JSON, in one store segment entry. A damaged segment is dropped and rebuilt, as before (§5.5).

## Measured

`bp index`, then `bp model --rev <pinned> --json`. Local, release build, warm cache, best of 5.

| Repository | First index | Reopen |
|---|---|---|
| small | 61 ms | 3 ms |
| medium | 182 ms | 21 ms |
| large | 1272 ms | 139 ms |

The 200 ms reopen budget is met on medium and large. CI reports the same measurement as `open_indexed`.

## Revisit when

- Indexing becomes per file for incremental re-indexing (M2): blob ids join tree ids as keys. The no-check argument still holds, because blob ids are content-addressed too.
- A forgotten `ANALYSER_VERSION` bump ships a stale model. If that happens, derive the version from the analysers' source instead.
