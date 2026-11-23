# Benchmarks

`bench/` measures the performance budgets of REQUIREMENTS §4. The method is in [decision 0002](decisions/0002-benchmark-method.md), and the reference repositories are in [decision 0001](decisions/0001-reference-repositories.md).

```
cargo build --release -p blueprint-cli -p blueprint-bench
target/release/bp-bench fetch       # shallow-fetch the pinned repos into bench/repos
target/release/bp-bench run --bp target/release/bp --out results.json \
    [--base-bp <bp built from another commit> --base-commit <sha>] [--summary summary.md]
```

`run` first checks that every reference checkout is clean and at its pinned commit. It then measures, and prints a markdown table of every measurement against its budget and against the base build, if one was given. After the table it lists the §4 budgets that cannot be measured yet.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Everything passed. |
| 1 | After re-measuring, something still exceeds its budget, is invalid, or is gated and more than 5% slower than base. |
| 2 | An error, such as a bad option, a missing or modified reference checkout, or a failed command. |

CI (`.github/workflows/bench.yml`) runs this on every pull request and every push to `main`, against the base commit built on the same runner. The table goes to the job summary and the JSON to the `bench-results` artifact. The agent copies the numbers into the agent log each session.

## Installed size

The 50 MB installed-size budget is measured by CI's `package` job (`.github/workflows/ci.yml`, #162, [decision 0016](decisions/0016-unsigned-installers.md)), not by `bp-bench`, because it needs the installer. On Linux the job unpacks the `.deb` and sums its files. On Windows it runs the NSIS installer silently and sums what it installed. Each number goes to the job summary, and the job fails over 50 MB (counting 1 MB as 1,000,000 bytes). First measurements (CI, PR #168): the `.deb` installs 37.9 MB, and the NSIS installer (8.6 MB) installs 26.5 MB.

## Resident memory

`memory_medium` is the §4 budget of 500 MB of resident memory with the medium repository open (#163). bp-bench reads it from the app's scripted UI session on the medium checkout, the same sessions as the interaction rows. The app's `--review-report` includes the process's peak resident memory (`VmHWM` from `/proc/self/status`) once the review is open, its map laid out and the script done. The row is the highest of the measured sessions, in MB of 1,000,000 bytes. Those sessions open a checkout indexed by an earlier session. `memory_medium_first_open` reports, without a budget, the session that indexes it into an empty cache, as a first open does. The row is Linux only: Windows' `GetProcessMemoryInfo` needs `unsafe`, which the workspace forbids, and the bench job runs on Linux. First local measurement (xvfb, release build): 261 MB, and 414 MB on the first open.

## Re-index after a one-file change

`reindex_one_file` is measured in process, with the engine resident, as the app will run it (DR 0008 decision 5). bp-bench builds the L1–L4 model of the pinned commit once with `engine::resident::Resident`. It then times applying an edit to one analysed Rust file: two blank lines added at the top and taken away again, so every item in the file moves. The budget row uses the file at the 90th percentile of size, a large but ordinary file. `reindex_one_file_p99` reports the 99th percentile, which on these repositories is generated code. `reindex_one_file_items` adds `pub fn new() {}` to the same p90 file and takes it away again. That is a change to the file's items, with a name many files mention, and it is held to the same budget. `reindex_one_file_root_items` makes the same edit to the crate root (`src/lib.rs`) at the 90th percentile of size among crate roots, held to the same budget (#141).

What is applied in place:
- **An edit inside items** (bodies, signatures, attributes): only the file's own nodes and calls change.
- **A change to a file's items or references (#81):** the file's nodes are made again. Its references are re-resolved, together with those of the other files that mention a changed name and every module segment a path into the file would have to name.

A change to `mod` declarations, to a crate's root file, or to any file other than Rust rebuilds the model.
