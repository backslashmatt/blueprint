#!/bin/bash
# Input-to-present latency from real X11 key events (#32): starts an instrumented build of the
# app (`--features latency`) on this repository under Xvfb, sends it arrow keys through the X
# server with xdotool, and prints its --latency-report.
# Usage: input-latency.sh <app> <out.json> [keys, default 60]
set -u
app=$1; out=$2; n=${3:-60}
APID=""; WPID=""; XPID=""
cleanup() { kill $APID $WPID $XPID 2>/dev/null; }
trap cleanup EXIT
trap 'cleanup; exit 1' INT TERM
if [ -z "${VK_ICD_FILENAMES:-}" ]; then
  for f in /usr/share/vulkan/icd.d/lvp_icd.json /usr/share/vulkan/icd.d/lvp_icd.x86_64.json; do
    [ -f "$f" ] && export VK_ICD_FILENAMES=$f && break
  done
fi
for d in $(seq 90 99); do [ -e "/tmp/.X11-unix/X$d" ] || break; done
Xvfb ":$d" -screen 0 1400x900x24 >/dev/null 2>&1 &
XPID=$!
sleep 1
export DISPLAY=":$d"
openbox >/dev/null 2>&1 &
WPID=$!
sleep 1
rm -f "$out"
"$app" . --latency-report "$out" --latency-samples "$n" &
APID=$!
win=""
for _ in $(seq 1 150); do
  win=$(xdotool search --pid "$APID" 2>/dev/null | head -1)
  [ -n "$win" ] && break
  sleep 0.2
done
[ -n "$win" ] || { echo "no window"; exit 1; }
sleep 3 # the review list loads
xdotool windowactivate --sync "$win" 2>/dev/null
xdotool windowfocus --sync "$win" 2>/dev/null
[ "$(xdotool getwindowfocus 2>/dev/null)" = "$win" ] || echo "warning: the app window does not have the focus"
# Half the keys move down, half back up, a key every 100 ms: each answered by one frame.
for _ in $(seq 1 $(( (n + 1) / 2 + 5 ))); do
  kill -0 "$APID" 2>/dev/null || break
  xdotool key Down; sleep 0.1
  xdotool key Up; sleep 0.1
done
wait "$APID"
status=$?
APID=""
[ -f "$out" ] || { echo "the app wrote no report (exit $status)"; exit 1; }
cat "$out"
