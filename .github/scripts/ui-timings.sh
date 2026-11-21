#!/bin/bash
# Runs the timed UI session (#95) on this repository and checks the §4 interaction budgets
# (check_ui_timings.py): open the review, drill three levels and back, open code and scroll
# it, then move about the map, pan and zoom.
# Usage: ui-timings.sh [wrapper...]   e.g. ui-timings.sh cage -- env -u DISPLAY
# The app is target/release/blueprint; the wrapper, if any, runs it (a compositor, say).
set -eu
app=target/release/blueprint
keys=end,b,home,h,enter,down,up,r,r,r,m,enter,enter,enter,backspace,enter,c,pagedown,pagedown,down,n,p,right,escape,tab,right,left,down,up,-,ctrl-right,ctrl-down,=,=,0,backspace
# One unmeasured session indexes the repository first.
timeout 300 "$@" "$app" . --script end,b,home,h,enter --review-report warm.json
timeout 300 "$@" "$app" . --script "$keys" --review-report review.json --timings timings.json
cat timings.json
# Windows runners name the interpreter `python`.
py=python3
python3 --version >/dev/null 2>&1 || py=python
"$py" .github/scripts/check_ui_timings.py timings.json "$(echo "$keys" | tr ',' '\n' | wc -l)"
