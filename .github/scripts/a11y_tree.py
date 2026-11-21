"""Reads the Blueprint window's accessibility tree through AT-SPI, as a screen
reader on Linux does, and checks that one view is labelled (#118).

Usage: python3 a11y_tree.py <view> [timeout-seconds]
where <view> is one of: list, findings, map, outline, code.
Prints the tree and exits non-zero, naming what is missing, if the view's
labels are not there. Needs gir1.2-atspi-2.0 and a running AT-SPI bus."""
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402


def blueprint():
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app is not None and (app.get_name() or "").lower() == "blueprint":
            return app
    return None


def walk(node, depth=0, out=None, limit=400):
    out = [] if out is None else out
    if len(out) >= limit:
        return out
    out.append((depth, node.get_role_name(), node.get_name() or ""))
    for i in range(min(node.get_child_count(), 80)):
        child = node.get_child_at_index(i)
        if child is not None:
            walk(child, depth + 1, out, limit)
    return out


EXPECT = {
    # view: (role, how its name starts, the role of its items or None)
    "list": ("list", "Commits", "list item"),
    "findings": ("list", "Findings", "list item"),
    "map": ("panel", "Map of", None),
    "outline": ("tree", "Outline of the map", "tree item"),
    "code": (None, "Code of ", None),
}


def check(view, tree):
    role, name, item = EXPECT[view]
    problems = []
    if not any(r == "frame" and n == "Blueprint" for _, r, n in tree):
        problems.append('a window named "Blueprint"')
    if not any(n.startswith(name) and (role is None or r == role) for _, r, n in tree):
        problems.append(f'a {role or "node"} whose name starts "{name}"')
    if item:
        items = [n for _, r, n in tree if r == item]
        if not items:
            problems.append(f"{item}s")
        elif not all(n.strip() for n in items):
            problems.append(f"a name on every {item}")
    return problems


def main():
    view = sys.argv[1]
    deadline = time.time() + float(sys.argv[2] if len(sys.argv) > 2 else 30)
    tree, problems = [], ["the Blueprint application on the AT-SPI bus"]
    while time.time() < deadline:
        app = blueprint()
        if app is not None:
            tree = walk(app)
            problems = check(view, tree)
            if not problems:
                break
        time.sleep(0.5)
    for depth, role, name in tree[:60]:
        print("  " * depth + f"{role}: {name}")
    if problems:
        print(f"a11y tree ({view}): missing " + "; ".join(problems), file=sys.stderr)
        sys.exit(1)
    print(f"a11y tree ({view}): labelled")


main()
