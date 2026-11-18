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
