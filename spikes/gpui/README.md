# GPUI spike (issue #13)

Throwaway measurement tool for the M0 UI toolkit decision. It is not a product crate and is excluded from the workspace.

`gpui-spike graph|text [--out metrics.json] [--hold-ms N]` opens a 1280×800 window and drives one scripted scene with no human in the loop:

- `graph`: 5,000 nodes and 10,000 edges. 300 frames of pan and zoom, then 30 simulated inputs.
- `text`: opens 50,000 highlighted lines, scrolls 300 frames, then 30 simulated inputs.

It then prints JSON and quits. The JSON holds first paint after process start, open time, per-frame CPU work (render start to end of paint), frame interval, input-to-paint time and resident memory.

Environment switches used while investigating:

| Variable | Effect |
|---|---|
| `SPIKE_EDGES_QUADS` | Draws edges as orthogonal 1 px quads instead of one stroked path |
| `SPIKE_NO_HL` | Turns off text highlighting |
| `SPIKE_FONT` | Sets the font family |
| `SPIKE_DEBUG` | Traces each render |

On Linux without a desktop, run it with `xvfb-run.sh`: Xvfb plus openbox. GPUI's X11 backend starts its frame loop only once a window manager reports the window as visible.

`patches/xattr` is a one-line fix that GPUI 0.2.2 needs to build against current `libc` on Linux.
