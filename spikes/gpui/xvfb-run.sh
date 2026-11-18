#!/bin/bash
# Runs a command under Xvfb with a window manager (GPUI's X11 backend starts its
# frame loop only once a WM reports the window visible). Usage: xvfb-run.sh <cmd...>
set -u
# Lavapipe (software Vulkan): Ubuntu and Fedora name its manifest differently.
if [ -z "${VK_ICD_FILENAMES:-}" ]; then
  for f in /usr/share/vulkan/icd.d/lvp_icd.json /usr/share/vulkan/icd.d/lvp_icd.x86_64.json; do
    [ -f "$f" ] && export VK_ICD_FILENAMES=$f && break
  done
fi
pkill -f "Xvfb :99" 2>/dev/null; sleep 0.2
Xvfb :99 -screen 0 1920x1080x24 >/dev/null 2>&1 &
XPID=$!
# Always tear down the display and WM, even if this script is killed by `timeout`.
trap 'kill $WPID $XPID 2>/dev/null' EXIT INT TERM
sleep 1
export DISPLAY=:99
openbox >/dev/null 2>&1 &
WPID=$!
sleep 1
"$@"
