# The Blueprint app

`blueprint [<repo>]` opens a window. Given a git repository, it shows the **review list**. Without a repository, it shows an empty window. Opening a repository from a folder dialog comes later in M3 (#90).

The app reaches the engine only through the protocol (`crates/protocol`). The engine runs on its own threads (`engine::service`, #89), so the window never waits on git, parsing or diffing.

## Review list

The list shows the repository's branches, remote branches, tags and pull request refs (`refs/pull/*`), followed by the 200 most recent commits from `HEAD`.

The base and head start at the suggested review:

- `HEAD` against its merge base with the default branch, which is where `origin/HEAD` points, else `main`, `master`, `origin/main` or `origin/master`, the first that exists. On a local `main` that is ahead of `origin/main`, that is what `main` adds;
- or, when that merge base is `HEAD` itself (or there is no default branch), `HEAD` against its first parent.

Everything is done from the keyboard:

| Key | Action |
|---|---|
| ↑ ↓ | Move between rows |
| Home End | First or last row |
| `b` | Use the row as the base |
| `h` | Use the row as the head |
| `s` | Swap base and head |
| Enter | Review base against head |
| Esc | Back to the list, from a review or a failed one |

A review first shows the systems and modules (L1–L2), then the full diff once the code levels are ready. For now it shows the number of findings and changed nodes; the map view (#92) replaces this.

## Options for tests and benchmarks

| Option | What it does |
|---|---|
| `--cold-start-report <file>` | Writes `{"cold_start_ms": …}` when the first frame is painted, then quits. bp-bench measures this as the `cold_start` budget. |
| `--script <keys>` | Presses comma-separated keys (GPUI key names such as `down`, `b`, `enter`) in the review list once it has loaded. |
| `--review-report <file>` | Once the review's full diff has arrived, writes `{"base", "head", "findings", "changed"}` and quits. |

CI runs `blueprint . --script end,b,home,h,enter --review-report review.json` on Ubuntu, Fedora and Windows: it opens this repository, chooses the oldest commit listed as base and the first ref (at `HEAD`) as head by keyboard, waits for the full review, and fails if base and head are the same commit.
