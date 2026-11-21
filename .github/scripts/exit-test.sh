#!/bin/bash
# The M4 exit test (REQUIREMENTS §6, #119): a review of one of the agent's own
# pull requests, end to end in the app with the keyboard only, exported as Markdown.
# Usage: exit-test.sh <base-commit> <head-commit> [wrapper...]
#   e.g. exit-test.sh <base> <head> spikes/gpui/xvfb-run.sh
# Writes exit-review.json (the app's review report) and exit-review.md (what `x`
# copied), and fails unless every step of the session had its effect.
set -eu
# Full ids, as the review report gives them.
base=$(git rev-parse --verify "$1^{commit}") head=$(git rev-parse --verify "$2^{commit}")
shift 2
app=${APP:-target/release/blueprint}
here=$(pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# A copy of the repository with only two branches, so the review list shows
# them first and in this order: a-base, then b-head. --no-deref deletes a
# symbolic ref (origin/HEAD) itself, not the branch it points at.
git clone -q --no-checkout "$here" "$work/repo"
git -C "$work/repo" for-each-ref --format='%(refname)' refs/heads refs/remotes refs/tags |
    while read -r r; do git -C "$work/repo" update-ref --no-deref -d "$r"; done
git -C "$work/repo" update-ref refs/heads/a-base "$base"
git -C "$work/repo" update-ref refs/heads/b-head "$head"
git -C "$work/repo" symbolic-ref HEAD refs/heads/b-head

# Review marks go to a fresh data directory, not the runner's.
export BLUEPRINT_DATA_DIR="$work/data"

# The session, by keys alone:
#   home,b,down,h,enter   choose base and head; the review opens on its findings
#   r,e,<note>,enter      mark the first finding's node reviewed, with a typed note
#   down×4,r              mark the fifth finding's node
#   m,enter,tab,r         on the map, drill into the selected node, select the next
#                         node there and mark it
#   x                     export the review to the clipboard
#   c,pagedown,n          open the selected node's code, page down, next change
note=c,h,e,c,k,e,d,space,b,y,space,k,e,y,b,o,a,r,d
keys="home,b,down,h,enter,r,e,$note,enter,down,down,down,down,r,m,enter,tab,r,x,c,pagedown,n"
timeout 300 "$@" "$here/$app" "$work/repo" --script "$keys" \
    --review-report "$here/exit-review.json" --export-report "$here/exit-review.md"
cat "$here/exit-review.json"

# Every step must have had its effect.
python3 - "$here/exit-review.json" "$here/exit-review.md" "$base" "$head" <<'PY'
import json, sys
report = json.load(open(sys.argv[1]))
export = open(sys.argv[2], encoding="utf-8").read()
base, head = sys.argv[3], sys.argv[4]
problems = []
def need(ok, what):
    if not ok:
        problems.append(what)
need((report.get("base"), report.get("head")) == (base, head),
     f"the review of {base[:10]}..{head[:10]} chosen by home,b,down,h")
need(f"Base `{base}`, head `{head}`" in export, "the export of that review")
need(report.get("marks") == 3, f"three marks made by r (got {report.get('marks')})")
need(report.get("notes") == 1, f"one note typed (got {report.get('notes')})")
need(report.get("code_rows", 0) > 0, "the code view open on a diff (c)")
need(report.get("code_cursor", 0) > report.get("code_opened_at", 0),
     "the code cursor moved on from where it opened (pagedown, n)")
need(export.startswith("## Blueprint review"), "the review copied by x")
need("checked by keyboard" in export, "the typed note in the export")
need(export.count("\n### Reviewed\n") == 1, "a Reviewed section in the export")
reviewed = export.split("### Reviewed", 1)[-1].split("###", 1)[0]
need(sum(1 for l in reviewed.splitlines() if l.startswith("- ")) == 3,
     "three reviewed nodes listed in the export")
if problems:
    print("exit test failed: missing " + "; ".join(problems), file=sys.stderr)
    print(export, file=sys.stderr)
    sys.exit(1)
print(f"exit test: {report['marks']} marks, {report['notes']} note, "
      f"{report['reviewed']} of {report['to_review']} changes reviewed, "
      f"{report['code_rows']} code rows; the export is in exit-review.md")
PY
