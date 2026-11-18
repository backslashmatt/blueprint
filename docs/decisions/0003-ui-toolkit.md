# 0003: UI toolkit — GPUI from Zed main, pinned

**Status:** accepted, 2026-11-18 (proposed 2026-11-15). The owner allowed the extra licences in #33. The corrected re-measurement (#13) did not settle the input-latency gap: it remains a known risk, carried to M3 as the revisit trigger in #32 (see Revisit when). **Issues:** #13 (spike), #14. **Evidence:** PR #21 (`spikes/gpui`, `.github/workflows/spike-gpui.yml`).

## Context

§5.2 makes GPUI the default toolkit but requires a time-boxed spike before building on it: a 5,000-node graph and a 50,000-line text view, on macOS, Windows and Linux, against the §4 budgets. §5.5 also requires screen-reader labels through the platform accessibility API.

## Options considered

1. **GPUI 0.2.2 from crates.io.** **Rejected.** It has no accessibility support at all (no AccessKit, no platform accessibility code), which fails §5.5. It was published in October 2025, and building it now needs a vendored fix to `xattr`.
2. **GPUI from Zed's main branch, pinned to a git revision.** **Chosen.**
   - Its source includes AccessKit integration (`role()`, `aria_label()`, and the macOS, Windows and Unix adapters), but the spike has **not exercised** it. It renders through Metal on macOS, Direct3D 11 on Windows and wgpu on Linux and web, and it has web and iOS back ends, so §5.1's later targets stay open.
   - The 20 Zed crates it reaches are Apache-2.0, and the zed-industries forks are MIT or Apache. **However**, the transitive set includes crates under licences not on §8.1's list: about 22 `Unicode-3.0` crates (`icu_*`, `zerovec` and others) and `tiny-keccak` (`CC0-1.0`). It also includes unreleased git pins of `wgpu` and `naga`. The owner allowed `Unicode-3.0` and `CC0-1.0` (#33; §8.1 wording in PR #55).
3. **Slint.** It has accessibility, but its licence is GPL-3.0 or Slint's own royalty-free or commercial licence, which needs the owner (§8.1). Not yet evaluated. §5.2 requires evaluating it if GPUI fails, and the input-latency gap below may count as such a failure.
4. **A custom `wgpu` renderer** with `winit` and `accesskit`. Kept as the fallback if a revisit trigger below fires.

## Decision

- Use GPUI from `github.com/zed-industries/zed`, pinned by revision (`e2f00a41ac67` at the time of writing), in `ui` and `app` only. The layering test already keeps UI crates out of `engine`. Upgrade the revision deliberately, in its own PR, with the spike scenes re-run.
- Move the workspace toolchain to Rust **1.98.1**, the version Zed pins, when GPUI first enters the workspace (M3).
- Draw map edges as **orthogonal segments made of quads**, not stroked paths. With paths, GPUI main timed out on Linux and Windows CI and used 542 MB on macOS; with quads, frames were fast everywhere. Orthogonal routing also suits the stable layout §5.4 asks for.
- Use concrete font families per platform. The generic `monospace` family does not resolve on macOS, Windows or Linux in GPUI main.

## Measured on GPUI main

Spike run 43691383463 on GitHub-hosted runners, with no real GPU on Linux or Windows. Graph rows use quad edges.

| | macOS (Metal) | Windows | Linux (Xvfb + llvmpipe) | Budget |
|---|---|---|---|---|
| First paint, graph / text | 148 / 177 ms | 123 / 220 ms | 71 / 100 ms | 300 ms |
| Open 50k-line view | 27 ms | 81 ms | 23 ms | 100 ms |
| CPU per frame p95, graph / text | 4.5 / 3.6 ms | 6.4 / 4.3 ms | 3.8 / 1.1 ms | 16 ms |
| Frame interval p50, graph / text | 16.7 / 16.7 ms | 31.7 / 31.7 ms (see Limits) | 16.2 / 16.0 ms | 16.7 ms |
| Input to paint p50 / p95, text | **19.5 / 22.4 ms** | **17.2 / 17.9 ms** | 8.7 / 16.1 ms | 16 ms |
| RSS, graph / text | 85 / 98 MB | 58 / 78 MB | 162 / 174 MB | — |
| Binary (release, line tables) | 10.6 MB | 10.7 MB | — | 50 MB installed |

## Re-measured on Linux and Windows

Spike run 43729297274, after the #13 corrections: 200 randomised inputs, the throttle disabled, a concrete monospace font, and a cold start for the text scene. macOS was not re-run (DR 0004).

| | Windows | Linux (Xvfb + llvmpipe) | Budget |
|---|---|---|---|
| First paint, text, cold | 414 ms | — | 300 ms |
| Open 50k-line view | 200 ms | — | 100 ms |
| Input to paint p50 / p95, text | 18 / 37 ms | — | 16 ms |
| Input to paint p50 / p95, graph (quads) | 32 ms / — | 15.6 / 16.7 ms | 16 ms |
| Frame interval p50, text / graph | 35 / 45 ms | — / 32 ms | 16.7 ms |
| CPU per frame p95, all scenes | ≤ 5.6 ms | ≤ 5.6 ms | 16 ms |

A dash means the value is in the run's job summary but was not copied here.

GPUI's own CPU work stays far inside budget. The misses come from presentation on software rasterisers, which CI cannot separate from GPUI. Real-GPU numbers need hardware; the owner deferred that check to M3 (#31, carried to the M3 epic #5). The latency revisit trigger (#32) stays open.

## Limits: what this does not show

- **Input latency** was measured from a timer-driven state change to the end of the next CPU paint. That leaves out OS event delivery and GPU present, and successive samples land at nearly the same vsync phase, so it is a lower bound. Even so, it is over budget on macOS and Windows (p50 17–20 ms).
- **Frame-interval tails miss 60 fps:** p95 is 20 ms on macOS and 43–48 ms on the software renderers.
- **The cold start was not truly cold.** Later scenes ran with warm caches. The text scenes used a fallback font, and the path-edge timeouts happened only under software rasterisers.
- **Windows' ~32 ms frame interval is not the inactive-window throttle.** The re-run below disabled the throttle (`inactive_frame_interval: None`), and the interval stayed at 35–45 ms. The runner draws with a software rasteriser, so Windows 60 fps on a real GPU is still untested.
- **Linux and Windows have no real GPU in CI.** Accessibility was seen initialising on Linux (`org.a11y.Bus`) but has not been checked end to end with a screen reader.
- Visual quality and smoothness need human eyes. The owner deferred this to the M3 owner checks (#31, #5).

## Why accept with the latency gap open

GPUI's CPU work is 3.6–6.4 ms per frame at p95, far inside the 16 ms budget, and GPUI 0.2.2 measured about 10 ms input-to-paint on the same macOS runner. That points to frame scheduling on GPUI main (rendering on the tick after the input arrives), not to a ceiling of the toolkit. The measured gap is a lower bound from timer-driven input, mostly on software rasterisers, so it is not yet evidence that GPUI fails §5.2. Evaluating Slint and a custom renderer now would cost more than finding out in M3, where real input and presentation can be instrumented. If #32's trigger fires, §5.2's evaluation of Slint and a custom `wgpu` renderer happens then, as a superseding record.

## Revisit when

- (Tracked in #32.) In M3, the instrumented input-to-present latency exceeds 16 ms on any platform and cannot be fixed within GPUI (for example by rendering on the input's own frame) within three attempts. Then evaluate option 4, and record a `needs-owner` budget question per §4.
- A pinned GPUI upgrade breaks the spike scenes or the AccessKit integration.
- Zed's licence for any reached crate changes from Apache-2.0.
