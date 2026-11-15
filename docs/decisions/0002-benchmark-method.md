# 0002: How benchmarks are measured and gated

**Status:** accepted, 2026-11-15. **Issue:** #12.

## Context

§9.1: no benchmark may regress by more than 5%, and no §4 budget may be exceeded. §9.3: benchmarks run in CI on a fixed runner class and are compared with the budget and with the last `main` result. Wall-clock timings are noisy, and GitHub-hosted runners are a label, not fixed hardware: two runs of the same label can land on different machines.

## Options considered

1. **Cached baseline.** Save each `main` result and compare later runs with it. Rejected after review (PR #19): it compares numbers from different machines and different times; a missing, stale or unparseable baseline silently disables the gate; and one red `main` run freezes the baseline for good.
2. **Same-run A/B.** Build the base commit and the commit under test in the same job and measure both, interleaved, on the same machine. Machine-to-machine differences cancel out. **Chosen.**
3. **Instruction counts** (e.g. with `perf`) instead of time. Deterministic, but hosted runners do not reliably expose hardware counters, and §4 budgets are in milliseconds. Kept as the fallback if A/B noise proves too high.

## Decision

1. **Base.** On a PR, the PR's base commit. On a push to `main`, the previous `main` commit, so a regression that reaches `main` shows as a red run on it, and the next run compares with its own parent: there is nothing to get stuck. A manual run has no base and checks budgets only; the table then says "not compared".
2. **Value = fastest sample.** After an untimed warm-up, each sample is a batch of calls (40 process spawns for `cli_process_start`, so one sample is about 50 ms) and the value is the fastest sample. Head and base samples alternate, and so does which goes first.
3. **Gate.** A gated measurement fails if it is more than 5% slower than its base. There is no absolute noise floor. A measurement fails regardless of gating if it exceeds its budget, is not finite, or is in a different unit from its budget.
4. **Confirmation.** If anything fails, the suite is measured again, up to twice, keeping the fastest values. Only a reproducible failure fails the run.
5. **Reference measurements** that measure the environment rather than Blueprint (the I/O floor) are reported but not gated.
6. **Runner class.** `ubuntu-24.04` hosted runners. The job prints the CPU count with every result.

## Measured

In a 4-CPU Linux container, `cli_process_start` (≈1.2 ms), comparing two builds of the same source, 8 runs each:

| Method | Spread of head vs base |
|---|---|
| batch 20, head always first | −1.7% … +3.9% |
| batch 40, head always first | −2.9% … +3.8% (head slower in 7 of 8: an order bias) |
| batch 40, alternating order | **−3.4% … +3.1%**, centred on 0 |

A real slowdown of about 0.9 ms (one extra `exec`) measured +77% and failed. The previous design, with a 2 ms noise floor, would have passed it.

## Limits

- The §4 reference machine is a 4-core, 16 GB laptop; a hosted runner may be smaller. Budgets are judged on the CI number, which is conservative if the runner is slower.
- The margin between measured noise (±3.4%) and the gate (5%) is narrow. Confirmation re-runs make a spurious failure unlikely, not impossible.
- Frame-time budgets need the instrumented UI build (§9.3, M3) and are not measured here.

## Revisit when

- Two consecutive spurious failures occur on CI: move gated benchmarks to instruction counts or larger batches.
- A self-hosted or larger runner class becomes available (issue #15).
