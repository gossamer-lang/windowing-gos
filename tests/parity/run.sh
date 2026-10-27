#!/bin/sh
# Runs the offscreen fixtures on the bytecode VM, the JIT, a debug native
# build, and a release native build, and reports whether the four
# transcripts agree byte for byte with each other and with expected.txt.
#
#   tests/parity/run.sh            compare
#   tests/parity/run.sh --update   rewrite expected.txt from the VM run
set -e
cd "$(dirname "$0")"
GOS=${GOS:-gos}
export GOS
../../tools/check-toolchain.sh
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

GOS_JIT=0 "$GOS" run . > "$out/vm.txt"
GOS_JIT=1 GOSSAMER_JIT_THRESHOLD=1 "$GOS" run . > "$out/jit.txt"
"$GOS" build . > /dev/null
./target/debug/windowing-parity > "$out/aot.txt"
"$GOS" build --release . > /dev/null
./target/release/windowing-parity > "$out/release.txt"

if [ "$1" = "--update" ]; then
    cp "$out/vm.txt" expected.txt
fi

status=0
for tier in jit aot release; do
    if ! diff -u "$out/vm.txt" "$out/$tier.txt"; then
        echo "parity: the VM and $tier transcripts disagree" >&2
        status=1
    fi
done
if ! diff -u expected.txt "$out/vm.txt"; then
    echo "parity: the transcript differs from expected.txt" >&2
    status=1
fi
[ $status -eq 0 ] && echo "parity: four tiers agree ($(wc -l < "$out/vm.txt") lines)"
exit $status
