#!/bin/bash
# Runs every spike scene and writes metrics (and, where possible, a screenshot)
# of each to out/. Used by .github/workflows/spike-gpui.yml on every runner,
# including the Fedora container. Exits 1 if a required scene fails.
set -u
mkdir -p out
bin=target/release/gpui-spike
failed=0
required_failed=0
# Text runs first: it is the cold first launch on this runner.
for name in text-default graph-quads graph-default; do
  scene=${name%%-*}
  extra=""; [ "$name" = graph-quads ] && extra="SPIKE_EDGES_QUADS=1"
  case "$RUNNER_OS" in
    Linux)
      env $extra timeout 200 ./xvfb-run.sh sh -c "$bin $scene --out out/$name.json --hold-ms 4000 & pid=\$!; \
        while kill -0 \$pid 2>/dev/null && [ ! -f out/$name.json ]; do sleep 0.2; done; \
        [ -f out/$name.json ] && sleep 1 && xwd -root -display :99 | convert xwd:- out/$name.png; wait \$pid" || { echo "::error::$name exited with failure"; failed=1; }
      ;;
    macOS)
      env $extra $bin $scene --out out/$name.json --hold-ms 4000 & pid=$!
      while kill -0 $pid 2>/dev/null && [ ! -f out/$name.json ]; do sleep 0.2; done
      [ -f out/$name.json ] && { sleep 1; screencapture -x out/$name.png || echo "::warning::screencapture failed"; }
      wait $pid || { echo "::error::$name exited with failure"; failed=1; }
      ;;
    Windows)
      env $extra $bin.exe $scene --out out/$name.json --hold-ms 4000 & pid=$!
      while kill -0 $pid 2>/dev/null && [ ! -f out/$name.json ]; do sleep 0.2; done
      if [ -f out/$name.json ]; then
        sleep 1
        powershell -NoProfile -Command "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
          \$b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; \$i=New-Object System.Drawing.Bitmap \$b.Width,\$b.Height; \
          \$g=[System.Drawing.Graphics]::FromImage(\$i); \$g.CopyFromScreen(\$b.Location,[System.Drawing.Point]::Empty,\$b.Size); \
          \$i.Save('out/$name.png')" || echo "::warning::screenshot failed"
      fi
      wait $pid || { echo "::error::$name exited with failure"; failed=1; }
      ;;
  esac
  if [ -f out/$name.json ]; then echo "$name: $(cat out/$name.json)"; else echo "::error::$name wrote no metrics"; failed=1; fi
  # Path-drawn edges are measured but not used (DR 0003); they are known to time out
  # under software rasterisers, so their failure is reported loudly but does not fail the job.
  if [ "$name" = graph-default ] && [ $failed = 1 ] && [ "$required_failed" = 0 ]; then
    echo "::warning::graph-default (path edges, not used by DR 0003) failed on $RUNNER_OS"; failed=0
  fi
  [ "$name" != graph-default ] && required_failed=$failed
done
ls -la target/release/gpui-spike* | head -3
exit $failed
