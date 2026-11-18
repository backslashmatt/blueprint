#!/bin/bash
# Screenshots the app's map under Xvfb with a window manager (REQUIREMENTS
# §9.4: rendering changes come with screenshots from the virtual display).
# The app is run with --review-report and --stay-open, and the screenshot is
# taken once it reports its map laid out, so it never shows a half-loaded
# screen; the step fails if that does not happen within the time given.
# Usage: screenshot.sh <out.png> <seconds to wait at most> <app> [app args...]
set -u
out=$1; limit=$2; shift 2
APID=""; WPID=""; XPID=""
cleanup() { kill $APID $WPID $XPID 2>/dev/null; }
trap cleanup EXIT INT TERM
if [ -z "${VK_ICD_FILENAMES:-}" ]; then
  for f in /usr/share/vulkan/icd.d/lvp_icd.json /usr/share/vulkan/icd.d/lvp_icd.x86_64.json; do
    [ -f "$f" ] && export VK_ICD_FILENAMES=$f && break
  done
fi
# A display nothing else is using.
for n in $(seq 90 99); do [ -e "/tmp/.X11-unix/X$n" ] || break; done
Xvfb ":$n" -screen 0 1400x900x24 >/dev/null 2>&1 &
XPID=$!
sleep 1
export DISPLAY=":$n"
openbox >/dev/null 2>&1 &
WPID=$!
sleep 1
report=$(mktemp)
rm -f "$report"
"$@" --review-report "$report" --stay-open >/dev/null 2>&1 &
APID=$!
for _ in $(seq "$limit"); do
  [ -s "$report" ] && break
  kill -0 "$APID" 2>/dev/null || { echo "the app exited before its map was ready"; exit 1; }
  sleep 1
done
[ -s "$report" ] || { echo "no map within $limit s"; exit 1; }
cat "$report"
sleep 1 # let the frame with the final layout be presented
timeout 30 import -window root "$out"
