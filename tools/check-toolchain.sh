#!/bin/sh
set -eu
GOS=${GOS:-gos}
version=$("$GOS" --version)
if ! printf '%s\n' "$version" | awk '
    { split($NF, v, "."); if ($NF !~ /^[0-9]+\.[0-9]+\.[0-9]+$/) exit 1;
      if (v[1]+0 > 0 || v[2]+0 > 75 || (v[2]+0 == 75 && v[3]+0 >= 5)) exit 0;
      exit 1 }'; then
    echo "Requires Gossamer >= 0.75.8; found $version. Set GOS to the compiler executable." >&2
    exit 1
fi
printf '%s\n' "$version"
