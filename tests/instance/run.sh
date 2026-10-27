#!/bin/sh
# Single-instance hand-off on the bytecode VM, the JIT, a debug native
# build, and a release native build, each on a private session bus: the
# first launch claims the name, the second forwards two paths and exits, and
# the first prints what it received.
set -e
cd "$(dirname "$0")/../../examples/single-instance"
GOS=${GOS:-gos}
export GOS
"$GOS" build . >/dev/null
"$GOS" build --release . >/dev/null
status=0
for tier in vm jit debug release; do
    out=$(dbus-run-session -- sh -c '
        case "$1" in
            vm) run() { GOS_JIT=0 "$GOS" run . "$@"; } ;;
            jit) run() { GOS_JIT=1 GOSSAMER_JIT_THRESHOLD=1 "$GOS" run . "$@"; } ;;
            debug) run() { ./target/debug/windowing-single-instance "$@"; } ;;
            release) run() { ./target/release/windowing-single-instance "$@"; } ;;
        esac
        log=$(mktemp)
        run > "$log" 2>&1 &
        primary=$!
        until grep -q "^primary$" "$log"; do
            kill -0 "$primary" 2>/dev/null || { cat "$log"; exit 1; }
            sleep 0.1
        done
        XDG_ACTIVATION_TOKEN=context-token DESKTOP_STARTUP_ID=context-startup run "/tmp/notes.txt" "/tmp/a b.txt"
        wait "$primary"
        cat "$log"
        rm -f "$log"
    ' sh "$tier" 2>&1) || true
    expected="forwarded
primary
request Open { uris: #[\"file:///tmp/notes.txt\", \"file:///tmp/a%20b.txt\"] }
activation context-token startup context-startup
open Some(\"/tmp/notes.txt\")
open Some(\"/tmp/a b.txt\")"
    if [ "$out" = "$expected" ]; then
        echo "  ok    $tier"
    else
        echo "  FAIL  $tier"
        echo "$out" | sed 's/^/        /'
        status=1
    fi
done
exit $status
