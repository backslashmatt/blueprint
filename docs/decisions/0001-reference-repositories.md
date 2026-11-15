# 0001: Reference repositories for benchmarks

**Status:** accepted, 2026-11-15. **Issue:** #12.

## Context

REQUIREMENTS §4 sets budgets against three reference repositories: small (10k lines), medium (500k lines) and large (5M lines), pinned by commit. M1 and M2 analyse Rust first (§3.4), so the references should be Rust-heavy, multi-crate workspaces where possible: the L1–L2 model is only exercised by repositories with many manifests.

## Options considered

Measured on shallow checkouts of each repository's default branch on 2026-11-15. A line is a line in a tracked text file (`git grep -I -c ''`).

| Candidate | Text lines | Files | Notes |
|---|---|---|---|
| `dtolnay/anyhow` | 6,643 | 54 | Too small, single crate |
| `tokio-rs/bytes` | 14,888 | 46 | Close to 10k, well known |
| `BurntSushi/memchr` | 253,698 | 184 | Inflated by benchmark data files |
| `rust-lang/rust-analyzer` | 696,846 | 2,389 | Large multi-crate workspace |
| `bevyengine/bevy` | 800,370 | 3,214 | Multi-crate, heavier on assets |
| `rust-lang/rust` | 6,398,809 | 64,483 | 4.1M lines of Rust; 59 MB shallow fetch |

## Decision

| Size | Repository | Commit | Text lines |
|---|---|---|---|
| small | `tokio-rs/bytes` | `f2cf483f13c9956a0f24ce2b7527f2318b0c8a2c` | 14,888 |
| medium | `rust-lang/rust-analyzer` | `b664fd5c7e7055c9569960f7fe73f21c7d00c9cd` | 696,846 |
| large | `rust-lang/rust` | `5f4effd579500175af844241eb179e756190f1e7` | 6,398,809 |

The medium and large picks are both above their nominal size (by 39% and 28%), which makes the budgets harder to meet, not easier. All three are MIT or Apache-2.0 licensed. They are fetched at the pinned commit only, never redistributed, and never built or executed (§5.5): `bp-bench fetch` disables hooks, fsmonitor, submodules and local-file transports, and writes `.git/info/attributes` so that no attribute-driven filter (LFS, or any smudge driver in the user's git config) runs on checkout; a test proves a configured filter does not run.

The pins live in `bench/repos.json`.

## Measured

On a 4-CPU Linux container, warm page cache, fastest of 11 runs: reading every file takes 0.3 ms (small), 21 ms (medium) and 340 ms (large). This is the I/O floor under the first-index budgets of 10 s and 120 s.

## Revisit when

- M5 adds TypeScript, C#, Python and Go. The large reference should then be checked for analyser coverage; a polyglot repository may be added beside it, not in place of it.
- A pinned repository becomes unavailable. The replacement keeps the size class, and a new record supersedes this one.
