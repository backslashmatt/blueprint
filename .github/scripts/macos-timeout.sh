#!/bin/bash
# macOS has no timeout(1). Puts a `timeout SECONDS COMMAND...` on the PATH for
# later steps: Perl's alarm survives exec, so the command is killed by SIGALRM
# (exit 142) if it runs longer. Used by the app steps and exit-test.sh (#133).
set -eu
if command -v timeout >/dev/null; then exit 0; fi
dir="$RUNNER_TEMP/timeout-bin"
mkdir -p "$dir"
cat > "$dir/timeout" <<'SH'
#!/bin/bash
secs=$1; shift
exec perl -e 'alarm shift; exec @ARGV or die "exec: $!"' "$secs" "$@"
SH
chmod +x "$dir/timeout"
echo "$dir" >> "$GITHUB_PATH"
