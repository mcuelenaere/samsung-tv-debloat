#!/bin/bash
# Start or restart dropbear on the TV. Run as root after each TV boot:
#   source /home/owner/share/tmp/sdk_tools/tv-start-sshd.sh
# Idempotent: safe to run again while dropbear is already running.
set -u

PUBKEY='REPLACE-WITH-YOUR-PUBLIC-KEY'
STAGING=/home/owner/share/tmp/sdk_tools
SRC=$STAGING/dropbear
LEGACY_HOSTKEY=$STAGING/dropbear_ed25519_host_key
DIR=/run/tv-sshd
# Dropbear rejects authorized_keys if any parent directory is group/other
# writable. /run and the userdata staging path fail that check; /opt does not.
STATE=/opt/tv-sshd
HOSTKEY=$STATE/dropbear_ed25519_host_key

[ -x "$SRC" ] || { echo "missing $SRC" >&2; exit 1; }

mkdir -p "$DIR" "$STATE"
chmod 700 "$DIR" "$STATE"
printf '%s\n' "$PUBKEY" > "$STATE/authorized_keys"
chmod 600 "$STATE/authorized_keys"

# Stop any previous instance (pidfile tracks the daemonized server).
if [ -f "$DIR/dropbear.pid" ]; then
    kill "$(cat "$DIR/dropbear.pid")" 2>/dev/null || true
    sleep 1
fi

# Executing a binary from the SMACK-labeled userdata mount is denied,
# so run it from /run, which is labeled System::Run and permits exec.
# The multi binary dispatches on argv[0] basename, so expose the
# sub-programs through symlinks.
cp "$SRC" "$DIR/dropbearmulti"
chmod 755 "$DIR/dropbearmulti"
ln -sf "$DIR/dropbearmulti" "$DIR/dropbear"
ln -sf "$DIR/dropbearmulti" "$DIR/dropbearkey"

# Keep the host key stable across TV power cycles. Migrate the key created
# by the first version of this launcher, or generate one on the TV itself.
if [ ! -s "$HOSTKEY" ] && [ -s "$LEGACY_HOSTKEY" ]; then
    cp "$LEGACY_HOSTKEY" "$HOSTKEY"
fi
if [ ! -s "$HOSTKEY" ]; then
    "$DIR/dropbearkey" -t ed25519 -f "$HOSTKEY" || exit 1
    echo "generated new ssh host key: $HOSTKEY"
fi
chmod 600 "$HOSTKEY"

# -D STATE    authorized_keys dir (root home is on the read-only rootfs)
# -s          public-key auth only (root's password is locked anyway)
# The exploit process leaves duplicate output pipes and device/socket handles
# open above fd 2. Close them in a detached subshell before exec; otherwise
# dropbear keeps the root agent's stdout/stderr open and the caller hangs.
# Redirections on the subshell protect fds 0-2 immediately after the fork.
(
    fd=3
    while [ "$fd" -lt 256 ]; do
        eval "exec ${fd}>&-" 2>/dev/null || true
        fd=$((fd + 1))
    done
    exec /usr/bin/setsid "$DIR/dropbear" \
        -D "$STATE" -r "$HOSTKEY" -s -p 22 -P "$DIR/dropbear.pid" -E
) </dev/null >>"$DIR/dropbear.log" 2>&1 &

sleep 2
echo "dropbear pid: $(cat "$DIR/dropbear.pid" 2>/dev/null || echo none)"
netstat -tln 2>/dev/null | grep ':22 ' || echo "port 22 not listening"
echo "--- last log lines ---"
tail -5 "$DIR/dropbear.log" 2>/dev/null
