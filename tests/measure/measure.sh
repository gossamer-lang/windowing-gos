#!/usr/bin/env bash
# The resource measurements of the package docs ("Resource use"): idle RSS,
# threads, and wakeups with one window on the software path and with a GL
# context, and event throughput on every tier, each beside the same program
# in C on the same SDL build. Linux; needs a display (DISPLAY or
# WAYLAND_DISPLAY) for the idle cases.
#
#   tests/measure/measure.sh [seconds]           the idle window, 60 by default
#   tests/measure/measure.sh --gate [seconds]    also fail above the budgets
#
# The budgets compare with the C programs on the same machine, so they hold
# on any hardware: the idle software window at most 32 MiB, 2 threads and
# one wakeup a second; the GL window within 25% (plus 8 MiB) of C's memory;
# native polling at most 8 times C's and JIT events at most 20 times.
# With MEASURE_BASELINE naming a release, native events cost no more than in
# that release, built here and run in turn with this one on the same machine;
# the program sends the events it receives, so this covers both directions.
# Each gated throughput is the fastest of several runs taken in turn with C's.
set -euo pipefail
cd "$(dirname "$0")"
GOS=${GOS:-gos}
gate=no
if [ "${1:-}" = --gate ]; then
    gate=yes
    shift
fi
window=${1:-60}
events=1000000

"$GOS" build --release . > /dev/null
gos_bin=./target/release/windowing-measure
# The C programs link the SDL the core crate builds, the version and features
# the binding links, from the output directory Cargo reports for its build.
prefix=$(cargo build --release --quiet --message-format=json \
    --manifest-path ../../rust/gos-windowing/Cargo.toml |
    jq -r 'select(.reason == "build-script-executed" and (.package_id | contains("#sdl3-sys@"))) | .out_dir')
cflags=$(PKG_CONFIG_PATH="$prefix/lib/pkgconfig" pkg-config --cflags sdl3)
libs=$(PKG_CONFIG_PATH="$prefix/lib/pkgconfig" pkg-config --static --libs sdl3)
out=$(mktemp -d)
trap 'git worktree remove --force "$out/baseline" 2>/dev/null || true; rm -rf "$out"' EXIT
base_bin=
if [ -n "${MEASURE_BASELINE:-}" ]; then
    git worktree add -q --detach "$out/baseline" "$MEASURE_BASELINE"
    (cd "$out/baseline/tests/measure" && "$GOS" build --release . > /dev/null)
    base_bin=$out/baseline/tests/measure/target/release/windowing-measure
fi
cc -O2 $cflags c/idle.c -o "$out/idle" $libs
cc -O2 $cflags c/throughput.c -o "$out/throughput" $libs

# Voluntary context switches over every thread: each is a thread waking
# from a blocking wait.
wakeups() {
    local pid=$1 total=0 n
    for status in /proc/"$pid"/task/*/status; do
        n=$(awk '/^voluntary_ctxt_switches/ {print $2}' "$status" 2>/dev/null || echo 0)
        total=$((total + ${n:-0}))
    done
    echo "$total"
}

# Starts `$@`, waits for its "ready" line, samples, and quits it.
idle_case() {
    local label=$1
    shift
    coproc PROG { exec "$@"; }
    local pid=$PROG_PID line
    read -r line <&"${PROG[0]}"
    [ "$line" = ready ] || { echo "$label: expected ready, got '$line'" >&2; exit 1; }
    local rss threads before after
    rss=$(awk '/^VmRSS/ {print $2}' /proc/"$pid"/status)
    threads=$(awk '/^Threads/ {print $2}' /proc/"$pid"/status)
    before=$(wakeups "$pid")
    sleep "$window"
    after=$(wakeups "$pid")
    # A coprocess of a non-interactive bash 5.3 starts with SIGINT ignored,
    # and SDL then leaves it alone; SIGTERM reaches SDL's quit event on every
    # bash.
    kill -TERM "$pid"
    wait "$pid"
    last_rss=$((rss / 1024))
    last_threads=$threads
    last_wakeups=$(awk -v d=$((after - before)) -v s="$window" 'BEGIN {printf "%.2f", d / s}')
    printf '| %s | %d | %d | %s |\n' "$label" "$last_rss" "$last_threads" "$last_wakeups"
}

failures=0
budget() {
    local what=$1 condition=$2
    if [ "$gate" = yes ] && ! awk "BEGIN { exit !($condition) }"; then
        echo "over budget: $what ($condition)" >&2
        failures=$((failures + 1))
    fi
}

echo "| Program | RSS (MiB) | Threads | Wakeups/s |"
echo "|---|---|---|---|"
idle_case "Gossamer, software" "$gos_bin" idle software
budget "idle software RSS" "$last_rss <= 32"
budget "idle software threads" "$last_threads <= 2"
budget "idle software wakeups" "$last_wakeups <= 1.0"
idle_case "C, software" "$out/idle" software
idle_case "Gossamer, GL" "$gos_bin" idle gl
gl_rss=$last_rss
idle_case "C, GL" "$out/idle" gl
budget "idle GL RSS against C" "$gl_rss <= $last_rss * 1.25 + 8"

echo
echo "| Tier | ns/event |"
echo "|---|---|"
per_event() { "$@" | awk '{print $NF}'; }
# Field `$1` of a throughput line, the word after its name.
field() { awk -v name="$1" '{ for (i = 1; i < NF; i++) if ($i == name) print $(i + 1) }'; }
export SDL_VIDEO_DRIVER=offscreen
# A busy machine slows a run and never speeds one up, so a program's fastest
# run is its cost; taking the runs in turn gives each the same machine.
rounds=5
fastest() {
    if [ -z "$1" ] || [ "$2" -lt "$1" ]; then echo "$2"; else echo "$1"; fi
}
vm=$(GOS_RUN_ENGINE=vm GOS_JIT=0 per_event "$GOS" run . throughput $events)
jit= native= native_poll= c= c_poll= base=
for _ in $(seq "$rounds"); do
    jit=$(fastest "$jit" "$(GOS_RUN_ENGINE=vm per_event "$GOS" run . throughput $events)")
    line=$("$gos_bin" throughput $events)
    native=$(fastest "$native" "$(awk '{print $NF}' <<< "$line")")
    native_poll=$(fastest "$native_poll" "$(field poll <<< "$line")")
    line=$("$out/throughput" $events)
    c=$(fastest "$c" "$(awk '{print $NF}' <<< "$line")")
    c_poll=$(fastest "$c_poll" "$(field poll <<< "$line")")
    if [ -n "$base_bin" ]; then
        base=$(fastest "$base" "$(per_event "$base_bin" throughput $events)")
    fi
done
echo "| bytecode VM | $vm |"
echo "| JIT | $jit |"
echo "| native (release) | $native |"
echo "| C | $c |"
echo "| native polling | $native_poll |"
echo "| C polling | $c_poll |"
budget "native polling against C" "$native_poll <= $c_poll * 8"
if [ -n "$base_bin" ]; then
    echo "| native (release $MEASURE_BASELINE) | $base |"
    budget "native ns/event against $MEASURE_BASELINE" "$native <= $base * 1.05"
fi
budget "JIT ns/event against C" "$jit <= $c * 20"
if [ "$failures" -gt 0 ]; then
    exit 1
fi
