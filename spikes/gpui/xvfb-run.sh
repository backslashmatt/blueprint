#!/bin/bash
# Runs the spike under Xvfb with a window manager. Usage: run-spike.sh <binary> <args...>
set -u
export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}
Xvfb :99 -screen 0 1920x1080x24 >/dev/null 2>&1 &
XPID=$!
sleep 1
export DISPLAY=:99
openbox >/dev/null 2>&1 &
WPID=$!
sleep 1
"$@"
RC=$?
kill $WPID $XPID 2>/dev/null
exit $RC
