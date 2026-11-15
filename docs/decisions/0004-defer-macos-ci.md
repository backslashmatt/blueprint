# 0004: macOS CI deferred

**Status:** accepted, 2026-11-15. **Issue:** #15. **Decided by:** the owner, in their own words on #15 (comment 6896728238): "Let's defer MacOs ci for now."

## Context

REQUIREMENTS §5.1, §6 and §8.2 require CI on macOS, Windows and Linux, both for every merge and for every milestone exit test. The repository is private, so hosted macOS runners consume Actions minutes at a 10× multiplier (#15). §8.1 reserves spending decisions for the owner.

## Decision

- Remove `macos-latest` from `.github/workflows/ci.yml`, and from the spike workflow when that lands (PR #21). `bench.yml` already runs on Linux only.
- Until the owner reinstates macOS, CI runs on Linux and Windows, and that is what the §8.2 merge rule and the §6 exit tests are checked against.
- **This defers macOS; it does not drop it.** §5.1 still makes macOS a required platform.
  - Code stays portable: no Linux- or Windows-only assumption without a `cfg` and a comment.
  - Each milestone's agent-log entry states that macOS was not checked in CI.
  - The project is not finished (REQUIREMENTS §11) until its milestones also pass on macOS.

## Measured

Before the deferral, macOS CI was green for every merged PR (#16–#20, #34). The GPUI spike's macOS numbers are in DR 0003 and on #13.

## Revisit when

- The owner reinstates macOS. Then add `macos-latest` back to the matrix.
- Before any milestone is declared complete for the final time (REQUIREMENTS §11).
