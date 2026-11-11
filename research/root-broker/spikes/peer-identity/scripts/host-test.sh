#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if [ "$(uname -s)" != "Linux" ]; then
    echo "SKIP: SO_PEERCRED/SO_PEERSEC host test requires Linux" >&2
    exit 77
fi

binary=${1:-"$root/target/debug/peer-identity"}
if [ ! -x "$binary" ]; then
    cargo build --locked --manifest-path "$root/Cargo.toml"
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/peer-identity-test.XXXXXX")
socket="$work/probe.sock"
server_log="$work/server.log"
client1_log="$work/client1.log"
client2_log="$work/client2.log"
server_pid=""

cleanup() {
    if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM

fail() {
    echo "FAIL: $*" >&2
    echo "--- server log ---" >&2
    cat "$server_log" >&2 2>/dev/null || true
    echo "--- client 1 log ---" >&2
    cat "$client1_log" >&2 2>/dev/null || true
    echo "--- client 2 log ---" >&2
    cat "$client2_log" >&2 2>/dev/null || true
    exit 1
}

require_log() {
    log=$1
    expected=$2
    grep -F "$expected" "$log" >/dev/null || fail "$log does not contain: $expected"
}

"$binary" server "$socket" 2 >"$server_log" 2>&1 &
server_pid=$!

attempt=0
while [ ! -S "$socket" ]; do
    kill -0 "$server_pid" 2>/dev/null || fail "server exited before creating its socket"
    attempt=$((attempt + 1))
    [ "$attempt" -le 100 ] || fail "timed out waiting for socket"
    sleep 0.05
done

"$binary" client "$socket" probe-one >"$client1_log" 2>&1 &
client1_pid=$!
wait "$client1_pid" || fail "first client failed"

"$binary" client "$socket" probe-two >"$client2_log" 2>&1 &
client2_pid=$!
wait "$client2_pid" || fail "second client failed"

wait "$server_pid" || fail "server failed"
server_pid=""

uid=$(id -u)
gid=$(id -g)
require_log "$server_log" "connection=1 so_peercred=present pid=$client1_pid uid=$uid gid=$gid "
require_log "$server_log" 'probe="probe-one"'
require_log "$server_log" "connection=2 so_peercred=present pid=$client2_pid uid=$uid gid=$gid "
require_log "$server_log" 'probe="probe-two"'
require_log "$client1_log" "client_pid=$client1_pid probe=\"probe-one\" reply=\"ok\""
require_log "$client2_log" "client_pid=$client2_pid probe=\"probe-two\" reply=\"ok\""

[ "$(wc -l <"$server_log")" -eq 3 ] || fail "expected one listening line and two connection records"
if grep -F 'so_peersec=error' "$server_log" >/dev/null; then
    fail "SO_PEERSEC returned an unexpected error"
fi

present=$(grep -c 'so_peersec=present' "$server_log" || true)
unsupported=$(grep -c 'so_peersec=unsupported' "$server_log" || true)
missing=$(grep -c 'so_peersec=missing' "$server_log" || true)
known=$((present + unsupported + missing))
[ "$known" -eq 2 ] || fail "could not account for both SO_PEERSEC results"

cat "$server_log"
cat "$client1_log"
cat "$client2_log"
echo "TRANSPORT/SO_PEERCRED CHECKS: PASSED (two distinct PIDs, expected UID/GID, correlated probes)"

if [ "$present" -eq 2 ]; then
    if [ -d /sys/kernel/security/smack ] || [ -d /sys/fs/smackfs ]; then
        echo "SMACK CHECK: SO_PEERSEC labels observed; package identity still requires two packaged TV apps"
    else
        echo "SMACK CHECK: NOT VALIDATED (SO_PEERSEC returned labels, but no SMACK filesystem was detected)"
    fi
elif [ "$unsupported" -gt 0 ]; then
    echo "SMACK CHECK: NOT VALIDATED (SO_PEERSEC is unsupported on this host)"
elif [ "$missing" -gt 0 ]; then
    echo "SMACK CHECK: NOT VALIDATED (SO_PEERSEC returned no security label)"
fi
