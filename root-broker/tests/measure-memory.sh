#!/usr/bin/env bash
# Read tv-authd/UI memory on the TV. With a size argument, also create a
# RAM-backed tmpfs file to put the system under controlled memory pressure.
# The pressure file is always removed on exit.
set -euo pipefail

TARGET=${TARGET:-root@172.17.2.110}
PRESSURE_MB=${1:-0}

if ! [[ "$PRESSURE_MB" =~ ^[0-9]+$ ]]; then
    echo "usage: $0 [pressure-mb]" >&2
    exit 2
fi
if [ "$PRESSURE_MB" -gt 512 ]; then
    echo "refusing more than 512 MiB of tmpfs pressure" >&2
    exit 2
fi

ssh -o BatchMode=yes -o ConnectTimeout=8 "$TARGET" "PRESSURE_MB=$PRESSURE_MB sh -s" <<'EOF'
set -eu
cleanup() { rm -f /dev/shm/tvauth-memhog; }
trap cleanup EXIT

show() {
    printf '\n== %s ==\n' "$1"
    grep -E '^(MemTotal|MemAvailable|MemFree):' /proc/meminfo
    pid=$(systemctl show tv-authd.service -p MainPID --value)
    if [ -n "$pid" ] && [ -r "/proc/$pid/status" ]; then
        printf 'tv-authd pid=%s ' "$pid"
        grep -E '^(VmRSS|VmHWM|Threads):' "/proc/$pid/status" | tr '\n' ' '
        printf '\n'
    else
        echo 'tv-authd: not running'
    fi
    uipid=$(pgrep -f '/opt/usr/apps/org.tvauth.ui/bin/tv-auth-ui' | head -1 || true)
    if [ -n "$uipid" ] && [ -r "/proc/$uipid/status" ]; then
        printf 'ui pid=%s ' "$uipid"
        grep -E '^(VmRSS|VmHWM|Threads):' "/proc/$uipid/status" | tr '\n' ' '
        printf '\n'
    else
        echo 'ui: not running (open a prompt to measure it)'
    fi
}

show before
if [ "$PRESSURE_MB" -gt 0 ]; then
    printf '\ncreating %s MiB tmpfs pressure...\n' "$PRESSURE_MB"
    dd if=/dev/zero of=/dev/shm/tvauth-memhog bs=1M count="$PRESSURE_MB" 2>/dev/null
    show during
    rm -f /dev/shm/tvauth-memhog
    show after
fi
EOF
