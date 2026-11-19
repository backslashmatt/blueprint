# The Blueprint app

`blueprint [<repo>]` opens a window. Given a git repository, it shows the **review list**. Without a repository, it shows an empty window. Ctrl+O (⌘O on macOS), or *Open a repository…* in the command palette, opens one from the platform's folder dialog, from any screen; it replaces whatever was open.

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

## Map

Reviewing opens the **map** (#92) at the systems: one level at a time, base and head drawn together, with the diff overlaid. It first shows the systems and modules (L1–L2), then the full diff once the code levels are ready, keeping your place.

- **Nodes** are filled by how they changed: green added, red removed, orange public surface, purple moved or renamed, yellow changed, grey contained or unchanged. An orange outline means something inside changed. `›` means the node has children to drill into.
- **Edges** are rolled up from the code (§3.2). A dependent is drawn above what it depends on. Green edges are new, red ones are gone, and grey ones are in both.
- **Layout** follows DR 0010. Base and head share one layout, and going back to a view keeps its nodes where they were.
- The view is centred on the selected node, which starts as the most changed one; selecting another node recentres it at the same zoom, and drilling resets pan and zoom. Only what is on screen is drawn.
- The map is built and laid out off the UI thread. A drill only reads its view's edges, which were rolled up once when the map was built: on medium, 0.03 ms on average and 3 ms at worst (`crates/ui/examples/map_measure.rs`).
- A contained change (body only) does not mark its ancestors, so the map stays quiet about it until you drill down to it (§3.3).

| Key | Action |
|---|---|
| ← → ↑ ↓ | Select the nearest node in that direction |
| Tab | Select the next node |
| Enter | Drill into the selected node, or open its code if nothing is inside it |
| `c` | Open the selected node's code (its file's diff, at its lines) |
| Backspace | Up one level; the breadcrumb is in the header |
| Ctrl+← → ↑ ↓ (⌘ on macOS) | Pan the view |
| `=` or `+`, `-` (also with Ctrl or ⌘, and the keypad + −) | Zoom in or out a step of 25%, about the middle of the view, from 26% to 381% |
| `0` | Back to the selection at 100% |
| Esc | Back to the review list |

## Code

Opening a node's code (#93) shows its file's diff side by side, base on the left and head on the right, scrolled to the node's lines (base's, for a node that is only in base). Removed lines are tinted red and added ones green; a changed line is tinted on both sides. The plain diff comes first, and the colours follow once the engine has highlighted the file (`open_50k_file`: 8 ms at 50,000 lines before highlighting). Only the rows on screen are drawn.

| Key | Action |
|---|---|
| ↑ ↓ PgUp PgDn Home End | Move |
| ← → | Scroll both sides sideways, for lines wider than their half |
| `n` `p` | Next or previous change |
| Backspace, Esc | Back to the map |

## Command palette

Every action has a key and an entry in the **command palette** (#94, §2.4). F1 (or Ctrl+Shift+P, ⌘⇧P on macOS) opens it over any screen, listing the actions of that screen with their keys. Typing filters them: titles that contain what you typed come first, then titles that contain its letters in order. ↑ ↓ choose, Enter runs the action, Esc closes the palette.

The actions, their titles, where they apply and their keys are one registry (`crates/ui/src/actions.rs`). The workspace runs keys through it, so a key without an action, or an action without a key or palette entry, cannot be added; a test checks every entry.

## Options for tests and benchmarks

| Option | What it does |
|---|---|
| `--cold-start-report <file>` | Writes `{"cold_start_ms": …}` when the first frame is painted, then quits. bp-bench measures this as the `cold_start` budget. |
| `--script <keys>` | Presses comma-separated keys (GPUI key names such as `down`, `b`, `enter`) once the review list has loaded. Keys after the one that opens a review wait for its full map, and each waits for the view it acts on to be laid out. |
| `--review-report <file>` | Once the review's full map is laid out and every `--script` key has been pressed, writes `{"base", "head", "findings", "changed", "map_nodes", "code_rows"}` and quits; with a code view open, it first waits for its highlighted diff. |
| `--timings <file>` | With `--review-report`: presses the `--script` keys one at a time, each once the last one's effect is painted, and writes `{"keys": [{"key", "action", "opened_code", "first_frame_ms", "settled_ms"}]}` with the report. `first_frame_ms` runs from the key to the end of the paint of the next frame; `settled_ms` to the end of the paint of the first frame drawn once the key's effect is complete (a drill's view laid out, a code view's diff arrived). Presenting the frame waits for the display and is not included. bp-bench turns these into the `drill`, `input_latency`, `frame_time` and `frame_time_120hz` rows (#95). |
| `--stay-open` | With `--review-report`: does not quit after writing it, so that a screenshot can be taken. |

CI runs `blueprint . --script end,b,home,h,enter --review-report review.json` on Ubuntu, Fedora and Windows: it opens this repository, chooses the oldest commit listed as base and the first ref (at `HEAD`) as head by keyboard, waits for the laid-out map, and fails if base and head are the same commit. It then repeats the review through the command palette alone, opening the code of the node it drills to, and fails if no code is shown. On Ubuntu it also uploads screenshots of the map, at the systems and drilled one level, of a code view and of the palette, as the `screenshots` artifact (`.github/scripts/screenshot.sh`).
