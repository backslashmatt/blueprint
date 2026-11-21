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

## Findings

A review opens on its **findings** (#114, §5.4 view 3), the reviewer's starting point, in the §3.3 order:

1. new dependencies between systems or modules, one row per pair;
2. new dependency cycles;
3. public surface changes: visible nodes whose signature or visibility changed (added and removed nodes are on the map, coloured);
4. change concentration: the ten nodes below the systems with the most changed lines of their own, so a change counts once, where it is, not again in every module above it.

Names below L1 come after their system's, as in `blueprint-store › segment`. Only the rows on screen are drawn.

| Key | Action |
|---|---|
| ↑ ↓ Home End | Choose a finding |
| Enter | Show it on the map: drilled to its node (a dependency's dependent, a cycle's first node) and selected |
| `r` | Mark the finding's node reviewed, or not (see Review state) |
| `e` | Write a note on the finding's node |
| `x` | Copy the review as Markdown |
| `m` | The map, at the systems |
| Esc | Back to the review list |

On the map, `f` goes back to the findings, where the highlighted row stays.

## Map

The **map** (#92), opened from the findings, shows the systems: one level at a time, base and head drawn together, with the diff overlaid. It first shows the systems and modules (L1–L2), then the full diff once the code levels are ready, keeping your place.

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
| `r` | Mark the selected node reviewed, or not (see Review state) |
| `e` | Write a note on the selected node |
| `x` | Copy the review as Markdown |
| Backspace | Up one level; the breadcrumb is in the header |
| `f` | Back to the findings |
| `o` | The outline tree, at the selected node |
| Ctrl+← → ↑ ↓ (⌘ on macOS) | Pan the view |
| `=` or `+`, `-` (also with Ctrl or ⌘, and the keypad + −) | Zoom in or out a step of 25%, about the middle of the view, from 26% to 381% |
| `0` | Back to the selection at 100% |
| Esc | Back to the review list |

## Outline

The **outline** (#117, §5.5) is the map as a tree, its non-visual equivalent with the findings list. `o` on the map opens it, expanded down to the map's view with the selected node highlighted. Each row gives the node's name, how it changed in words, whether it is reviewed, and how many of its siblings it depends on and is used by: the map's edges, counted. It is built from the map's own views, so every node the map can show is in it.

| Key | Action |
|---|---|
| ↑ ↓ Home End | Move |
| → | Expand the row, or go to its first child |
| ← | Collapse the row, or go to its parent |
| Enter, `o`, Esc | Back to the map, at the highlighted node |
| `r`, `e`, `x` | Mark the highlighted node reviewed, write a note on it, export the review |

## Code

Opening a node's code (#93) shows its file's diff side by side, base on the left and head on the right, scrolled to the node's lines (base's, for a node that is only in base). Removed lines are tinted red and added ones green; a changed line is tinted on both sides. The plain diff comes first, and the colours follow once the engine has highlighted the file (`open_50k_file`: 8 ms at 50,000 lines before highlighting). Only the rows on screen are drawn.

| Key | Action |
|---|---|
| ↑ ↓ PgUp PgDn Home End | Move |
| ← → | Scroll both sides sideways, for lines wider than their half |
| `n` `p` | Next or previous change |
| `r` | Mark the node whose code this is reviewed, or not (see Review state) |
| Backspace, Esc | Back to the map |

## Review state

`r` marks a changed node reviewed (#115, §5.4 view 5): on the map, the selected node; on the findings, the highlighted finding's node; in the code, the node it was opened from. Pressing it again on a node you marked unmarks it. The rules are in DR 0011:

- **Marking covers what is inside.** Marking a node reviewed also counts every changed node under it as reviewed: marking a system says you have reviewed it whole.
- **Roll-up.** A node is reviewed when every changed node under it is, unless something of its own changed: lines of its own, a move or rename, or a public surface change. A type whose signature changed is not reviewed because its methods are.
- **What shows it.** Reviewed nodes carry a ✓ on the map, findings whose node is reviewed carry one in the list, and the header says `reviewed <n> of <m>`, counting the changed nodes that only their own mark can make reviewed.
- **When it is available.** Marks need the full diff, so `r` works once the header says `complete`.
- **Where marks live.** They are kept on this computer, per repository (`bp review` reads and writes the same ones: [cli.md](cli.md)).
- **Rebases.** A mark applies wherever the node changed exactly as it did when marked, so it survives a rebase that leaves that change alone, and is dropped, never misapplied, when the change differs.

`x` copies the whole review to the clipboard as Markdown, ready to paste into a pull request comment (#116):

- the base and head commits and the progress;
- the findings in order, each ticked ✓ or ☐;
- the reviewed nodes, each at the highest level reviewed, with their notes;
- what is still to review, at the highest level nothing under has been reviewed;
- every other note.

`bp review export <repo> <base> <head>` prints the same text from the same stored marks.

`e` writes a note on the same node `r` would mark: a one-line editor opens with the node's note, Enter saves it (keeping whether the node is reviewed), Esc cancels, and saving an empty note removes it. The note of the node under the cursor is shown under the header, and `bp review` prints notes too.

## Accessibility

Blueprint works with the keyboard alone, and with a screen reader (#118, §5.5):

- **Screen readers.** The app exposes its screens to the platform's accessibility API (AccessKit through GPUI):
  - the review list, the findings and the outline are named lists, and the outline is a tree;
  - each row is read without its cursor mark, says whether it is the highlighted row and its place in the list, and for the outline, its level and whether it is expanded;
  - the highlighted row, or the map, is the focused window's active item, so moving the cursor is read out;
  - the map is a named region whose label says where it is, how many nodes it shows, and the selected node's change, review state and edges;
  - notices carry the alert role. GPUI cannot yet mark them live, so macOS and Windows screen readers find them but do not speak them unprompted.

  The findings list and the outline together are the map's non-visual equivalent.
- **Contrast.** Every pair of colours the app draws meets WCAG AA in light and dark themes: 4.5:1 for text, including code colours on changed rows, and 3:1 for map edges. A test checks every pair, map node outlines included (3:1). Added and removed edges differ by colour only; the outline and the findings list say the same without colour.
- **Motion.** There are no animations or transitions. The view changes when you press a key, or all at once when more of the diff arrives. So there is nothing to turn off for reduced motion.
- **Text size.** Text and row heights follow the system's text-size setting: GNOME's *Large Text* / text-scaling factor on Linux (on an X11 session GNOME already applies it through the display's DPI, which the app follows), and *Make text bigger* on Windows. Other Linux desktops have no shared setting; use their display scaling or the override below. macOS has no system-wide text size for desktop apps, so there the size is fixed. The setting is read when the app starts; restart it after changing it. `BLUEPRINT_TEXT_SCALE=1.5` overrides it (0.5 to 3). The map keeps its own zoom (`+`/`-`), since its boxes are laid out in pixels. Page Up and Page Down in the code view move a screenful at the current size. Rows that do not fit end in an ellipsis; the chosen row is shown in full on a *Selected:* line under the commit list, the findings and the outline. Lists always keep at least two rows in view; when the text above them is taller than that allows, the screen scrolls (with the mouse wheel or trackpad, not yet from the keyboard). Display scaling is followed as well.

## Command palette

Every action has a key and an entry in the **command palette** (#94, §2.4). F1 (or Ctrl+Shift+P, ⌘⇧P on macOS) opens it over any screen, listing the actions of that screen with their keys. Typing filters them: titles that contain what you typed come first, then titles that contain its letters in order. ↑ ↓ choose, Enter runs the action, Esc closes the palette.

The actions, their titles, where they apply and their keys are one registry (`crates/ui/src/actions.rs`). The workspace runs keys through it, so a key without an action, or an action without a key or palette entry, cannot be added; a test checks every entry.

## Latency

An instrumented build, `cargo build --release -p blueprint-app --features latency`, turns on GPUI's profiler. With `--latency-report <file> [--latency-samples <n>]` it records, for each frame that answers input, the time from the dispatch of the first input event to the end of presenting that frame (#32), and once `n` frames have (60 by default) writes `{"samples", "p50_ms", "p95_ms", "p99_ms", "max_ms", "mid_draw_events_dropped", "draw_p50_ms", "draw_p95_ms", "dirty_to_present_p50_ms", "dirty_to_present_p95_ms"}` and quits. It is a lower bound on input to photons: it leaves out the time an event waits before the app dispatches it, and the compositor and display after presenting.

`.github/scripts/input-latency.sh` drives it with real X11 key events (xdotool under Xvfb), and CI reports the result without gating on it. CI runners have no GPU: presenting means lavapipe rasterizing the whole window on the CPU, about 11 ms of the 12 ms median, while building the frame (`draw`) takes under 1 ms. The §4 budget of 16 ms from input to painted frame is checked on real hardware (#96):

```
cargo run --release -p blueprint-app --features latency -- <repo> --latency-report latency.json --latency-samples 60
```

then press ↓ and ↑ alternately, 60 times in all, in the review list; the app quits once it has the samples.

## Options for tests and benchmarks

- `--export-report <file>` (with `--review-report`): writes to `<file>`, when the report is written, the Markdown the last `x` copied; the file is empty if `x` was not pressed. The review report also gives `marks` (marks made reviewed) and `notes` (notes typed).

| Option | What it does |
|---|---|
| `--cold-start-report <file>` | Writes `{"cold_start_ms": …}` when the first frame is painted, then quits. bp-bench measures this as the `cold_start` budget. |
| `--script <keys>` | Presses comma-separated keys (GPUI key names such as `down`, `b`, `enter`) once the review list has loaded. Keys after the one that opens a review wait for its full map, and each waits for the view it acts on to be laid out. |
| `--review-report <file>` | Once the review's full map is laid out and every `--script` key has been pressed, writes `{"base", "head", "findings", "changed", "map_nodes", "code_rows"}` and quits; with a code view open, it first waits for its highlighted diff. |
| `--timings <file>` | With `--review-report`: presses the `--script` keys one at a time, each once the last one's effect is painted, and writes `{"keys": [{"key", "action", "opened_code", "first_frame_ms", "settled_ms"}]}` with the report. `first_frame_ms` runs from the key to the end of the paint of the next frame; `settled_ms` to the end of the paint of the first frame drawn once the key's effect is complete (a drill's view laid out, a code view's diff arrived). Presenting the frame waits for the display and is not included. bp-bench turns these into the `drill`, `input_latency`, `frame_time` and `frame_time_120hz` rows (#95). |
| `--stay-open` | With `--review-report`: does not quit after writing it, so that a screenshot can be taken. |

CI runs `blueprint . --script end,b,home,h,enter --review-report review.json` on Ubuntu, Fedora and Windows: it opens this repository, chooses the oldest commit listed as base and the first ref (at `HEAD`) as head by keyboard, waits for the laid-out map, and fails if base and head are the same commit. It then repeats the review through the command palette alone, opening the code of the node it drills to, and fails if no code is shown. On Ubuntu it also uploads screenshots of the findings, of the map, at the systems and drilled one level, of a code view and of the palette, as the `screenshots` artifact (`.github/scripts/screenshot.sh`).
