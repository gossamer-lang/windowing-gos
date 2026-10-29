#!/bin/sh
# Real foreground handoff with two native processes, including Wayland tokens.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
out=${WINDOWING_INSTANCE_REPORT:-$(mktemp -d /tmp/windowing-instance.XXXXXX)}
mkdir -p "$out"
out=$(cd "$out" && pwd)
if [ "${1:-}" != --private ]; then
    (cd "$root/rust/gos-windowing" && cargo build --release --example instance-check --example platform-check) > "$out/build.txt" 2>&1
    case "$(uname -s)" in
        Linux) WINDOWING_INSTANCE_REPORT="$out" sh "$root/tools/private-bus.sh" -- "$0" --private ;;
        *) "$root/rust/gos-windowing/target/release/examples/instance-check" > "$out/native.txt"
           "$root/rust/gos-windowing/target/release/examples/platform-check" > "$out/platform-native.txt" ;;
    esac
    exit
fi
# A socket path is at most 108 bytes, so the runtime directory sits at a short
# path of its own.
runtime=$(mktemp -d /tmp/windowing-rt.XXXXXX)
export XDG_RUNTIME_DIR="$runtime" XDG_CONFIG_HOME="$out/config"
command -v Xvfb >/dev/null
Xvfb -displayfd 3 -screen 0 1280x900x24 -nolisten tcp 3> "$out/xvfb-display" > "$out/xvfb.log" 2>&1 &
xvfb=$!
server=""
trap 'kill "$xvfb" ${server:+"$server"} 2>/dev/null || :; wait "$xvfb" 2>/dev/null || :; [ -z "$server" ] || wait "$server" 2>/dev/null || :; rm -rf "$runtime"' EXIT HUP INT TERM
attempt=0
until [ -s "$out/xvfb-display" ]; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 100 ] || ! kill -0 "$xvfb" 2>/dev/null; then cat "$out/xvfb.log" >&2; exit 1; fi
    sleep 0.1
done
export DISPLAY=":$(cat "$out/xvfb-display")"
cc -Wall -Wextra -Werror -O2 "$root/tests/instance/focus-seat.c" -lX11 -o "$out/focus-seat"
command -v sway >/dev/null
cat > "$out/publish-env.sh" <<'SCRIPT'
printf '%s\n' "$WAYLAND_DISPLAY" > "$WINDOWING_INSTANCE_REPORT/wayland-display"
printf '%s\n' "$DISPLAY" > "$WINDOWING_INSTANCE_REPORT/x11-display"
SCRIPT
cat > "$out/sway.conf" <<'CONFIG'
xwayland force
focus_follows_mouse no
focus_on_window_activation focus
default_border pixel 1
for_window [app_id=".*"] floating enable
for_window [class=".*"] floating enable
exec sh "$WINDOWING_INSTANCE_REPORT/publish-env.sh"
CONFIG
WLR_BACKENDS=x11 WLR_RENDERER=pixman WINDOWING_INSTANCE_REPORT="$out" sway --unsupported-gpu --config "$out/sway.conf" --debug > "$out/sway.log" 2>&1 &
server=$!
attempt=0
while [ ! -s "$out/wayland-display" ] || [ ! -s "$out/x11-display" ]; do
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 100 ] || ! kill -0 "$server" 2>/dev/null; then cat "$out/sway.log" >&2; exit 1; fi
    sleep 0.1
done
export WAYLAND_DISPLAY="$(cat "$out/wayland-display")"
display=$(cat "$out/x11-display")
NATIVE_INSTANCE_TIME=$(timeout 5 "$out/focus-seat" 0)
export NATIVE_INSTANCE_TIME
export DISPLAY=$display
binary="$root/rust/gos-windowing/target/release/examples/instance-check"
SDL_VIDEO_DRIVER=wayland WAYLAND_DEBUG=client "$binary" > "$out/wayland.txt" 2> "$out/wayland-protocol.txt"
grep -q 'xdg_activation_v1.*activate(' "$out/wayland-protocol.txt"
SDL_VIDEO_DRIVER=x11 "$binary" > "$out/x11.txt" 2> "$out/x11.stderr"
platform="$root/rust/gos-windowing/target/release/examples/platform-check"
SDL_VIDEO_DRIVER=wayland "$platform" > "$out/platform-wayland.txt" 2> "$out/platform-wayland.stderr"
SDL_VIDEO_DRIVER=x11 "$platform" > "$out/platform-x11.txt" 2> "$out/platform-x11.stderr"
echo "Native single-instance activation: X11 and Wayland; report $out"
