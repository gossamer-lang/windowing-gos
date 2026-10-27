#!/bin/sh
# Runs the Wayland drag fixture on the bytecode VM, the JIT, a debug native
# build, and a release native build, twice each: dropping on its own window,
# and on a second process (the release build in `--target` mode) that reads
# the data from it. Each runs as a client of a private Weston
# (kiosk shell, so the window fills the output at a known place) whose input
# comes from a private X server (Xephyr nested in the current display), where
# XTest presses and moves the pointer.
set -e
cd "$(dirname "$0")"
GOS=${GOS:-gos}
export SDL_VIDEO_DRIVER=wayland
for tool in Xephyr weston; do
    command -v "$tool" > /dev/null || { echo "wldnd: needs $tool" >&2; exit 1; }
done
if [ -z "$DISPLAY" ]; then
    echo "wldnd: Xephyr needs a display to nest in" >&2
    exit 1
fi
input=:96
# A Wayland socket path must fit in 108 bytes, so the runtime directory is short.
# A socket path is at most 108 bytes, so the runtime directory sits at a short
# path of its own.
runtime=$(mktemp -d /tmp/wldnd-rt.XXXXXX)
chmod 700 "$runtime"
Xephyr "$input" -screen 640x480 -ac > /dev/null 2>&1 &
xephyr=$!
compositor=
trap 'kill $compositor $xephyr 2>/dev/null; rm -rf "$runtime"' EXIT
while [ ! -e "/tmp/.X11-unix/X${input#:}" ]; do sleep 0.1; done
DISPLAY="$input" XDG_RUNTIME_DIR="$runtime" weston --backend=x11-backend.so --shell=kiosk-shell.so \
    --socket=wldnd --width=640 --height=480 --idle-time=0 --no-config > "$runtime/weston.txt" 2>&1 &
compositor=$!
while [ ! -S "$runtime/wldnd" ]; do sleep 0.1; done
export XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=wldnd WLDND_INPUT_DISPLAY="$input"
"$GOS" build . > /dev/null
"$GOS" build --release . > /dev/null
status=0
check() {
    label=$1
    expected=$2
    shift 2
    if out=$(timeout -k 5 30 "$@" 2>&1) && echo "$out" | grep -q "^$expected: hello drag$" && echo "$out" | grep -q "^finished: Some(Copy)$"; then
        echo "  ok    $label"
    else
        echo "  FAIL  $label"
        echo "$out" | sed 's/^/        /'
        status=1
    fi
}
target="$PWD/target/release/windowing-wldnd"
for mode in local cross; do
    if [ "$mode" = cross ]; then expected="target dropped"; drop="WLDND_TARGET=$target"; else expected=dropped; drop="WLDND_TARGET="; fi
    check "vm $mode" "$expected" env $drop GOS_JIT=0 "$GOS" run .
    check "jit $mode" "$expected" env $drop GOS_JIT=1 GOSSAMER_JIT_THRESHOLD=1 "$GOS" run .
    check "debug $mode" "$expected" env $drop ./target/debug/windowing-wldnd
    check "release $mode" "$expected" env $drop ./target/release/windowing-wldnd
done
exit $status
