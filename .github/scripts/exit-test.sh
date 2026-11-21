#!/bin/bash
# The M4 exit test (REQUIREMENTS §6, #119): a review of one of the agent's own
# pull requests, end to end in the app with the keyboard only, exported as Markdown.
# Usage: exit-test.sh <base-commit> <head-commit> [wrapper...]
# Writes exit-review.json and exit-review.md, and fails unless the session marked
# nodes reviewed and the export carries the note typed into it.
set -eu
base=$1 head=$2
shift 2
app=${APP:-target/release/blueprint}
here=$(pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# A copy of the repository with only two branches, so the review list shows
# them first and in this order: a-base, then b-head.
git clone -q --no-checkout "$here" "$work/repo"
git -C "$work/repo" for-each-ref --format='%(refname)' refs/heads refs/remotes refs/tags |
    while read -r r; do git -C "$work/repo" update-ref -d "$r"; done
git -C "$work/repo" update-ref refs/heads/a-base "$base"
git -C "$work/repo" update-ref refs/heads/b-head "$head"
git -C "$work/repo" symbolic-ref HEAD refs/heads/b-head

# Review marks go to a fresh data directory, not the runner's.
export BLUEPRINT_DATA_DIR="$work/data"

# Choose base and head, open the review on its findings, mark the first
# finding reviewed with a note, mark the fifth, then on the map drill down, mark
# a node, read its code a page at a time, and export.
note=c,h,e,c,k,e,d,space,b,y,space,k,e,y,b,o,a,r,d
keys="home,b,down,h,enter,r,e,$note,enter,down,down,down,down,r,m,enter,enter,r,c,pagedown,n,escape,x"
timeout 300 "$@" "$here/$app" "$work/repo" --script "$keys" \
    --review-report "$here/exit-review.json" --export-report "$here/exit-review.md"
cat "$here/exit-review.json"

reviewed=$(sed -E 's/.*"reviewed": ([0-9]+).*/\1/' "$here/exit-review.json")
if [ "$reviewed" -lt 2 ]; then
    echo "exit test: expected at least 2 nodes reviewed, got $reviewed" >&2
    exit 1
fi
grep -q "checked by keyboard" "$here/exit-review.md" || {
    echo "exit test: the export does not carry the typed note" >&2
    cat "$here/exit-review.md" >&2
    exit 1
}
grep -q "^### Reviewed" "$here/exit-review.md"
echo "exit test: $reviewed nodes reviewed; the export is in exit-review.md"
