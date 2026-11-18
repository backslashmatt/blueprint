#!/bin/bash
# Screenshots the app under Xvfb with a window manager (REQUIREMENTS §9.4:
# rendering changes come with screenshots from the virtual display).
# Usage: screenshot.sh <out.png> <seconds to wait> <app> [app args...]
set -u
out=$1; wait=$2; shift 2
if [ -z "${VK_ICD_FILENAMES:-}" ]; then
  for f in /usr/share/vulkan/icd.d/lvp_icd.json /usr/share/vulkan/icd.d/lvp_icd.x86_64.json; do
    [ -f "$f" ] && export VK_ICD_FILENAMES=$f && break
  done
fi
Xvfb :97 -screen 0 1400x900x24 >/dev/null 2>&1 &
XPID=$!
trap 'kill $APID $WPID $XPID 2>/dev/null' EXIT INT TERM
sleep 1
export DISPLAY=:97
openbox >/dev/null 2>&1 &
WPID=$!
sleep 1
"$@" >/dev/null 2>&1 &
APID=$!
sleep "$wait"
import -window root "$out"
