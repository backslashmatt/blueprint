#!/bin/bash
# macOS has no timeout(1). Puts a `timeout SECONDS COMMAND...` on the PATH for
# later steps, for the app steps and exit-test.sh (#133). Like GNU timeout it
# runs the command in its own process group and, if it runs longer, kills the
# whole group and exits 124; otherwise it exits with the command's status.
set -eu
if command -v timeout >/dev/null; then exit 0; fi
dir="$RUNNER_TEMP/timeout-bin"
mkdir -p "$dir"
cat > "$dir/timeout" <<'PL'
#!/usr/bin/perl
use strict; use warnings;
my $secs = shift @ARGV;
@ARGV or die "usage: timeout SECONDS COMMAND...\n";
my $pid = fork() // die "fork: $!\n";
if ($pid == 0) {
    setpgrp(0, 0);
    # The list form with an explicit program never goes through a shell.
    exec { $ARGV[0] } @ARGV or die "exec $ARGV[0]: $!\n";
}
my $timed_out = 0;
local $SIG{ALRM} = sub { $timed_out = 1; kill 'TERM', -$pid; sleep 2; kill 'KILL', -$pid; };
alarm $secs;
waitpid($pid, 0);
my $status = $?;
alarm 0;
exit 124 if $timed_out;
exit($status & 127 ? 128 + ($status & 127) : $status >> 8);
PL
chmod +x "$dir/timeout"
echo "$dir" >> "$GITHUB_PATH"
