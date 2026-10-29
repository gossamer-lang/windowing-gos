#!/bin/sh
# Runs a command on a private session bus that starts only the D-Bus services
# named before `--`. A host's desktop services (portals, gvfs) never join it,
# so none of them answers a test or outlives the run.
# Usage: private-bus.sh [SERVICE...] -- COMMAND [ARG...]
set -eu
config=$(mktemp -d)
trap 'rm -rf "$config"' EXIT HUP INT TERM
mkdir "$config/services"
while [ "$#" -gt 0 ] && [ "$1" != -- ]; do
    found=
    for dir in /usr/local/share/dbus-1/services /usr/share/dbus-1/services; do
        if [ -f "$dir/$1.service" ]; then found=$dir/$1.service; break; fi
    done
    if [ -z "$found" ]; then echo "private-bus.sh: this host has no D-Bus service $1" >&2; exit 2; fi
    # The bus starts the service as its own child, not through the host's systemd.
    grep -v '^SystemdService=' "$found" > "$config/services/$1.service"
    shift
done
if [ "$#" -lt 2 ]; then echo 'usage: private-bus.sh [SERVICE...] -- COMMAND [ARG...]' >&2; exit 2; fi
shift
cat > "$config/session.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <servicedir>$config/services</servicedir>
  <policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy>
</busconfig>
CONF
dbus-run-session --config-file="$config/session.conf" -- "$@"
