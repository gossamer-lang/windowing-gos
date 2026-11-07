#!/bin/sh
# Runs the release smoke build on real video drivers without touching the
# user's session: X11 on a private server (Xvfb, or Xephyr nested in the
# current display) and Wayland on a headless Weston compositor, or headless
# sway where no Weston is packaged.
set -e
# No session bus: with none named, the first D-Bus client would autolaunch one
# that outlives the run and holds the captured output open.
export DBUS_SESSION_BUS_ADDRESS=disabled:
cd "$(dirname "$0")"
GOS=${GOS:-gos}
"$GOS" build --release . >/dev/null
binary=./target/release/windowing-smoke
status=0
scratch=$(mktemp -d)
trap 'kill $servers 2>/dev/null; rm -rf "$scratch" ${runtime:+"$runtime"}' EXIT
servers=""

# The program's own `smoke:` line is the result; drivers and compositors print
# warnings around it, which are kept only when the program printed nothing.
report() {
    line=$(printf '%s\n' "$3" | grep '^smoke:' | tail -n 1)
    [ -n "$line" ] || line=$3
    if [ "$2" -eq 0 ]; then echo "  ok    $1: $line"; else echo "  FAIL  $1: $line"; status=1; fi
}

display=:96
if command -v Xvfb >/dev/null 2>&1; then
    Xvfb "$display" -screen 0 640x480x24 >/dev/null 2>&1 &
    servers="$servers $!"
elif command -v Xephyr >/dev/null 2>&1 && [ -n "$DISPLAY" ]; then
    Xephyr "$display" -screen 640x480 -ac >/dev/null 2>&1 &
    servers="$servers $!"
else
    display=""
fi
if [ -n "$display" ]; then
    while [ ! -e "/tmp/.X11-unix/X${display#:}" ]; do sleep 0.1; done
    out=$(env -u WAYLAND_DISPLAY DISPLAY="$display" SDL_VIDEO_DRIVER=x11 "$binary" 2>&1) && code=0 || code=$?
    report x11 "$code" "$out"
else
    echo "  FAIL  x11: needs Xvfb, or Xephyr and a display"
    status=1
fi

if command -v weston >/dev/null 2>&1; then
    # A socket path is at most 108 bytes, so the runtime directory sits at a short
    # path of its own.
    runtime=$(mktemp -d /tmp/smoke-rt.XXXXXX)
    XDG_RUNTIME_DIR="$runtime" weston --backend=headless --socket=gos-smoke >/dev/null 2>&1 &
    servers="$servers $!"
    while [ ! -e "$runtime/gos-smoke" ]; do sleep 0.1; done
    out=$(env -u DISPLAY XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=gos-smoke SDL_VIDEO_DRIVER=wayland "$binary" 2>&1) && code=0 || code=$?
    report wayland "$code" "$out"
elif command -v sway >/dev/null 2>&1; then
    # FreeBSD packages no Weston; sway's wlroots headless backend stands in.
    runtime=$(mktemp -d /tmp/smoke-rt.XXXXXX)
    config="$scratch/sway.config"
    : >"$config"
    XDG_RUNTIME_DIR="$runtime" WLR_BACKENDS=headless WLR_RENDERER=pixman \
        WLR_LIBINPUT_NO_DEVICES=1 sway -c "$config" >/dev/null 2>&1 &
    servers="$servers $!"
    while [ ! -S "$runtime/wayland-1" ]; do sleep 0.1; done
    out=$(env -u DISPLAY XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=wayland-1 SDL_VIDEO_DRIVER=wayland "$binary" 2>&1) && code=0 || code=$?
    report wayland "$code" "$out"
else
    echo "  FAIL  wayland: needs weston or sway"
    status=1
fi
exit $status
