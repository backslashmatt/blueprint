#!/usr/bin/env python3
"""Checks a `blueprint --timings` file against the REQUIREMENTS §4 interaction
budgets, the way bp-bench's `ui_session` turns timings into rows (#95):

- drill: the slowest drill that did not open code, key to settled frame;
- input_latency: the slowest key to its first painted frame;
- frame_time and frame_time_120hz: the slowest scroll, move, pan or zoom key
  to its first painted frame.

Usage: check_ui_timings.py <timings.json> <expected number of keys>
Exits 1 if a budget is exceeded or the session went off script.
"""

import json
import sys

SCROLLS = {
    "list.up", "list.down", "list.first", "list.last",
    "map.left", "map.right", "map.up", "map.down", "map.next",
    "map.pan_left", "map.pan_right", "map.pan_up", "map.pan_down",
    "map.zoom_in", "map.zoom_out", "map.zoom_reset",
    "code.up", "code.down", "code.page_up", "code.page_down", "code.first",
    "code.last", "code.next_change", "code.previous_change",
    "code.scroll_left", "code.scroll_right",
}
DRILLS = {"map.drill_down", "map.drill_up"}
BUDGETS = [  # (row, budget in ms), as in bench/src/budgets.rs
    ("drill", 50.0),
    ("input_latency", 16.0),
    ("frame_time", 16.7),
    ("frame_time_120hz", 8.3),
]


def main() -> int:
    path, expected = sys.argv[1], int(sys.argv[2])
    with open(path, encoding="utf-8") as f:
        keys = json.load(f)["keys"]
    if len(keys) != expected:
        print(f"the session timed {len(keys)} of {expected} keys: off script")
        return 1
    drills = [k["settled_ms"] for k in keys if k["action"] in DRILLS and not k["opened_code"]]
    scrolls = [k["first_frame_ms"] for k in keys if k["action"] in SCROLLS]
    if not drills or not scrolls:
        print("the session drilled or scrolled nothing: off script")
        return 1
    frame = max(scrolls)
    values = {
        "drill": max(drills),
        "input_latency": max(k["first_frame_ms"] for k in keys),
        "frame_time": frame,
        "frame_time_120hz": frame,
    }
    failed = False
    print("| Row | Value | Budget | Status |")
    print("|---|---|---|---|")
    for row, budget in BUDGETS:
        ok = values[row] <= budget
        failed |= not ok
        print(f"| {row} | {values[row]:.2f} ms | {budget} ms | {'ok' if ok else 'OVER'} |")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
