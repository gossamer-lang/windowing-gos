#!/bin/sh
# Runs the XDND drag fixture on the bytecode VM, the JIT, a debug native
# build, and a release native build, each against a private X server
# (Xvfb, or Xephyr nested in the current display), and checks every tier
# reports the drop.
set -e
cd "$(dirname "$0")"
GOS=${GOS:-gos}
export SDL_VIDEO_DRIVER=x11
display=:97
if command -v Xvfb >/dev/null 2>&1; then
    Xvfb "$display" -screen 0 900x400x24 >/dev/null 2>&1 &
elif command -v Xephyr >/dev/null 2>&1 && [ -n "$DISPLAY" ]; then
    Xephyr "$display" -screen 900x400 -ac >/dev/null 2>&1 &
else
    echo "xdnd: needs Xvfb, or Xephyr and a display" >&2
    exit 1
fi
server=$!
trap 'kill "$server" 2>/dev/null' EXIT
while [ ! -e "/tmp/.X11-unix/X${display#:}" ]; do sleep 0.1; done
export DISPLAY="$display"
"$GOS" build . >/dev/null
"$GOS" build --release . >/dev/null
status=0
check() {
    label=$1
    shift
    if out=$(timeout -k 5 30 "$@" 2>&1) && echo "$out" | grep -q "^dropped: hello drag$" && echo "$out" | grep -q "^finished: Some(Copy)$"; then
        echo "  ok    $label"
    else
        echo "  FAIL  $label"
        echo "$out" | sed 's/^/        /'
        status=1
    fi
}
check vm env GOS_JIT=0 "$GOS" run .
check jit env GOS_JIT=1 GOSSAMER_JIT_THRESHOLD=1 "$GOS" run .
check debug ./target/debug/windowing-xdnd
check release ./target/release/windowing-xdnd
exit $status
