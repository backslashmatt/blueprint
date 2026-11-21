#!/bin/bash
# The accessibility tree test (#118): each view of the app, read through AT-SPI
# as a screen reader on Linux reads it, must carry its labels. Runs every view
# in its own session bus with the AT-SPI bus up and accessibility switched on.
# Usage: a11y-tree.sh <base-commit> <head-commit> [wrapper...]
#   e.g. a11y-tree.sh <base> <head> spikes/gpui/xvfb-run.sh
# Needs at-spi2-core, gir1.2-atspi-2.0, python3-gi and dbus.
set -eu
base=$(git rev-parse --verify "$1^{commit}") head=$(git rev-parse --verify "$2^{commit}")
shift 2
app=${APP:-target/release/blueprint}
here=$(pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The same two-branch copy as the exit test: a-base, then b-head.
git clone -q --no-checkout "$here" "$work/repo"
git -C "$work/repo" for-each-ref --format='%(refname)' refs/heads refs/remotes refs/tags |
    while read -r r; do git -C "$work/repo" update-ref --no-deref -d "$r"; done
git -C "$work/repo" update-ref refs/heads/a-base "$base"
git -C "$work/repo" update-ref refs/heads/b-head "$head"
git -C "$work/repo" symbolic-ref HEAD refs/heads/b-head
export BLUEPRINT_DATA_DIR="$work/data"

# The system Python, which has the AT-SPI bindings (python3-gi).
py=${A11Y_PYTHON:-/usr/bin/python3}

open="home,b,down,h,enter"
failed=0
for view in list findings map outline code; do
    case $view in
        list) keys=home ;;
        findings) keys=$open ;;
        map) keys=$open,m ;;
        outline) keys=$open,m,o ;;
        code) keys=$open,m,enter,tab,c ;;
    esac
    echo "== $view ($keys)"
    report="$work/$view.json"
    # Past the list, the review report is written once the view has settled;
    # the window stays open for the tree to be read.
    log="$work/$view.log"
    if ! timeout 300 dbus-run-session -- bash -c '
        set -u
        /usr/libexec/at-spi-bus-launcher --launch-immediately >/dev/null 2>&1 &
        sleep 1
        for p in IsEnabled ScreenReaderEnabled; do
            dbus-send --session --print-reply --dest=org.a11y.Bus /org/a11y/bus \
                org.freedesktop.DBus.Properties.Set string:org.a11y.Status string:$p variant:boolean:true >/dev/null
        done
        report=$1 view=$2 keys=$3 here=$4 app=$5 repo=$6 py=$7 log=$8; shift 8
        # Its own process group (setsid), so the wrapper, the app, Xvfb and
        # the window manager all stop together at the end.
        if [ "$view" = list ]; then
            setsid "$@" "$here/$app" "$repo" --script "$keys" >"$log" 2>&1 &
        else
            setsid "$@" "$here/$app" "$repo" --script "$keys" --review-report "$report" --stay-open >"$log" 2>&1 &
        fi
        group=$!
        stop() { kill -TERM -- -"$group" 2>/dev/null; sleep 1; kill -KILL -- -"$group" 2>/dev/null; }
        if [ "$view" != list ]; then
            for i in $(seq 1 240); do [ -s "$report" ] && break; sleep 0.5; done
            if [ ! -s "$report" ]; then
                echo "a11y tree ($view): the view did not settle in 120 s" >&2
                stop; exit 1
            fi
        fi
        cd /tmp && "$py" "$here/.github/scripts/a11y_tree.py" "$view" 60; rc=$?
        stop
        exit $rc
    ' _ "$report" "$view" "$keys" "$here" "$app" "$work/repo" "$py" "$log" "$@"; then
        failed=1
        echo "-- the app's output for $view:" >&2
        tail -20 "$log" >&2 || true
    fi
done
[ $failed = 0 ] && echo "a11y tree: every view is labelled"
exit $failed
