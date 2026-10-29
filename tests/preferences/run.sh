#!/bin/sh
# Runs the preferences fixture on the bytecode VM, the JIT, a debug native
# build, and a release native build, each against a stand-in desktop portal
# on a private session bus.
set -e
cd "$(dirname "$0")"
GOS=${GOS:-gos}
if [ "${1:-}" = --private ]; then
    shift
    out=$(mktemp -d)
    trap 'rm -rf "$out"' EXIT
    mkfifo "$out/commands"
    python3 -I portal.py < "$out/commands" > "$out/portal.txt" &
    portal=$!
    exec 3> "$out/commands"
    until grep -q ready "$out/portal.txt"; do
        if ! kill -0 "$portal" 2>/dev/null; then
            echo "the stand-in portal exited before it was ready"
            cat "$out/portal.txt"
            exit 1
        fi
        sleep 0.05
    done
    "$@" > "$out/probe.txt" 2>&1 &
    probe=$!
    until grep -q "^first:" "$out/probe.txt" || ! kill -0 "$probe" 2>/dev/null; do sleep 0.05; done
    echo change >&3
    wait "$probe" || true
    exec 3>&-
    wait "$portal" || true
    cat "$out/probe.txt"
    exit
fi
"$GOS" build . > /dev/null
"$GOS" build --release . > /dev/null
first='first: Preferences { high_contrast: None, reduced_motion: Some(false), accent: Some((51, 102, 255)), text_scale: Some(1.25), menu_font_size: Some(11.0) }'
second='second: Preferences { high_contrast: Some(true), reduced_motion: Some(true), accent: Some((51, 102, 255)), text_scale: Some(1.25), menu_font_size: Some(13.0) }'
status=0
check() {
    label=$1
    shift
    out=$(sh ../../tools/private-bus.sh -- sh run.sh --private "$@" 2>/dev/null)
    if echo "$out" | grep -qxF "$first" && echo "$out" | grep -qxF "$second"; then
        echo "  ok    $label"
    else
        echo "  FAIL  $label"
        echo "$out" | sed 's/^/        /'
        status=1
    fi
}
check vm env GOS_JIT=0 "$GOS" run .
check jit env GOS_JIT=1 GOSSAMER_JIT_THRESHOLD=1 "$GOS" run .
check debug ./target/debug/windowing-preferences
check release ./target/release/windowing-preferences

# On X11 the display's content scale comes from its DPI where the desktop
# publishes one, and desktops fold the text scale into that DPI.
if [ -z "$DISPLAY" ] || ! command -v Xephyr > /dev/null; then
    echo "  skip  x11: needs Xephyr and a display to nest it in"
    exit $status
fi
folded='first: Preferences { high_contrast: None, reduced_motion: Some(false), accent: Some((51, 102, 255)), text_scale: None, menu_font_size: Some(11.0) }'
kept='first: Preferences { high_contrast: None, reduced_motion: Some(false), accent: Some((51, 102, 255)), text_scale: Some(1.25), menu_font_size: Some(11.0) }'
screen=:95
Xephyr $screen -screen 320x240 -ac -noreset > /dev/null 2>&1 &
xephyr=$!
manager=
trap 'kill $manager $xephyr 2>/dev/null' EXIT
while [ ! -e "/tmp/.X11-unix/X${screen#:}" ]; do sleep 0.05; done
until DISPLAY=$screen xdpyinfo > /dev/null 2>&1; do sleep 0.05; done
x11() {
    label=$1
    want=$2
    shift 2
    for tier in vm jit debug release; do
        case $tier in
            vm) probe="env GOS_JIT=0 $GOS run . x11" ;;
            jit) probe="env GOS_JIT=1 GOSSAMER_JIT_THRESHOLD=1 $GOS run . x11" ;;
            *) probe="./target/$tier/windowing-preferences x11" ;;
        esac
        out=$(env SDL_VIDEO_DRIVER=x11 DISPLAY=$screen "$@" sh ../../tools/private-bus.sh -- sh run.sh --private $probe 2>/dev/null)
        if echo "$out" | grep -qxF "$want"; then
            echo "  ok    x11 $label $tier"
        else
            echo "  FAIL  x11 $label $tier"
            echo "$out" | sed 's/^/        /'
            status=1
        fi
    done
}
# Starts a stand-in XSETTINGS manager with these arguments, or none.
settings() {
    if [ -n "$manager" ]; then
        kill $manager
        wait $manager 2>/dev/null || true
        manager=
    fi
    if [ $# -gt 0 ]; then
        ready=$(mktemp)
        DISPLAY=$screen python3 -I xsettings.py "$@" > "$ready" 2>&1 &
        manager=$!
        until grep -q ready "$ready"; do
            if ! kill -0 "$manager" 2>/dev/null; then
                echo "  FAIL  the stand-in XSETTINGS manager exited before it was ready"
                sed 's/^/        /' "$ready"
                rm -f "$ready"
                exit 1
            fi
            sleep 0.05
        done
        rm -f "$ready"
    fi
}
x11 "no dpi" "$kept"
settings --big Xft/DPI=147456
x11 "xsettings dpi" "$folded"
settings --little Gdk/WindowScalingFactor=2 Xft/DPI=294912
x11 "xsettings window scale" "$kept"
settings
echo "Xft.dpi: 144" | DISPLAY=$screen xrdb -merge
x11 "resource dpi" "$folded"
x11 "forced scale" "$kept" SDL_VIDEO_X11_SCALING_FACTOR=1.5
exit $status
