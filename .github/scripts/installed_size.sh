#!/usr/bin/env bash
# Reports an installed size against the 50 MB budget (REQUIREMENTS §4, #162) and fails over it.
# Usage: installed_size.sh <what> <bytes>. MB is 1,000,000 bytes, the stricter reading.
set -euo pipefail
what=$1
bytes=$2
budget=50000000
mb=$(awk -v b="$bytes" 'BEGIN { printf "%.1f", b / 1000000 }')
line="installed size ($what): $mb MB of 50 MB"
echo "$line"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "- $line" >> "$GITHUB_STEP_SUMMARY"; fi
if [ "$bytes" -gt "$budget" ]; then
  echo "over the installed-size budget"
  exit 1
fi
