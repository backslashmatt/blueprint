# 0003: UI toolkit — GPUI from Zed main, pinned

**Status:** accepted, 2026-11-15. **Issues:** #13 (spike), #14. **Evidence:** PR #21 (`spikes/gpui`, `.github/workflows/spike-gpui.yml`).

## Context

§5.2 makes GPUI the default toolkit but requires a time-boxed spike before building on it: a 5,000-node graph and a 50,000-line text view, on macOS, Windows and Linux, against the §4 budgets. §5.5 also requires screen-reader labels through the platform accessibility API.

## Options considered

1. **GPUI 0.2.2 from crates.io.** **Rejected.** It has no accessibility support at all (no AccessKit, no platform accessibility code), which fails §5.5. It was published in October 2025, and building it now needs a vendored fix to `xattr`.
2. **GPUI from Zed's main branch, pinned to a git revision.** **Chosen.**
   - It has AccessKit integration (`role()`, `aria_label()`, and the macOS, Windows and Unix adapters), renders through wgpu, and has web and iOS back ends, so §5.1's later targets stay open.
   - All 22 Zed crates it reaches are Apache-2.0.
3. **Slint.** It has accessibility, but its licence is GPL-3.0 or Slint's own royalty-free or commercial licence, which needs the owner (§8.1). Not pursued, because option 2 passes.
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

## Limits: what this does not show

- **Input latency** was measured from a timer-driven state change to the end of the next paint, not from a real OS event to the presented frame. On macOS and Windows it lands just over one 60 Hz frame (17–22 ms). §9.3's instrumented input-to-present measurement arrives with the app in M3.
- **Windows' ~32 ms frame interval** is most likely the hosted runner's virtual display refreshing at about 30 Hz. That is not verified.
- **Linux and Windows have no real GPU in CI.** Accessibility was seen initialising on Linux (`org.a11y.Bus`) but has not been checked end to end with a screen reader.
- Visual quality and smoothness need human eyes: see #31.

## Revisit when

- (Tracked in #32.) In M3, the instrumented input-to-present latency exceeds 16 ms on any platform and cannot be fixed within GPUI (for example by rendering on the input's own frame) within three attempts. Then evaluate option 4, and record a `needs-owner` budget question per §4.
- A pinned GPUI upgrade breaks the spike scenes or the AccessKit integration.
- Zed's licence for any reached crate changes from Apache-2.0.
