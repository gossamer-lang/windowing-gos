#!/bin/sh
# Formatting and lint over the library, every example, and both test
# projects. Needs no display.
#
#   ./quick-check.sh          report only
#   ./quick-check.sh --fix    rewrite what `gos fmt` and `gos lint --fix` can
set -e
cd "$(dirname "$0")"
GOS=${GOS:-gos}
export GOS
./tools/check-toolchain.sh

fix=no
[ "$1" = "--fix" ] && fix=yes

status=0
log=$(mktemp)
trap 'rm -f "$log"' EXIT

run() {
    label=$1
    shift
    if "$@" >"$log" 2>&1; then
        printf '  ok    %s\n' "$label"
    else
        printf '  FAIL  %s\n' "$label"
        sed 's/^/        /' "$log"
        status=1
    fi
}

for dir in . examples/*/ tests/parity tests/smoke tests/measure tests/xdnd tests/wldnd tests/preferences tests/platform tests/oledrag tests/uia; do
    [ -f "$dir/project.toml" ] || continue
    name=${dir%/}
    echo "$name"
    if [ "$fix" = yes ]; then
        run "fmt" sh -c "cd '$name' && '$GOS' fmt ."
        run "lint" sh -c "cd '$name' && '$GOS' lint --fix ."
    else
        run "fmt" sh -c "cd '$name' && '$GOS' fmt --check ."
        run "lint" sh -c "cd '$name' && '$GOS' lint ."
    fi
done

# The generated sources must match what their generators write now.
echo "generated"
keys=$(mktemp)
trap 'rm -f "$log" "$keys"' EXIT
run "keys" sh -c "'$GOS' run tools/gen_keys.gos > '$keys' && cmp -s '$keys' src/keys.gos"

if [ "$status" -eq 0 ]; then
    echo
    echo "quick-check: clean"
else
    echo
    echo "quick-check: something needs attention" >&2
fi
exit "$status"
