#!/bin/bash
# Stage dropbear and its launcher on the TV through Samsung's sdb.
# Usage: ./install-dropbear.sh 192.0.2.10
# Optional: SDB=/path/to/tizen-studio/tools/sdb
set -euo pipefail

TV_IP=${1:?'usage: install-dropbear.sh TV_IP'}
SDB=${SDB:-$HOME/tizen-studio/tools/sdb}
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
BINARY=$SCRIPT_DIR/out/dropbear
LAUNCHER=$SCRIPT_DIR/tv-start-sshd.sh
DEVICE=$TV_IP:26101
STAGING=/home/owner/share/tmp/sdk_tools

[ -x "$SDB" ] || { echo "sdb not found: $SDB" >&2; exit 1; }
[ -x "$BINARY" ] || { echo "build first: $SCRIPT_DIR/build-dropbear.sh" >&2; exit 1; }

"$SDB" connect "$DEVICE"
"$SDB" -s "$DEVICE" push "$BINARY" "$STAGING/dropbear"
"$SDB" -s "$DEVICE" push "$LAUNCHER" "$STAGING/tv-start-sshd.sh"

cat <<EOF
Staged on $TV_IP. Start or restart it from a root shell on the TV with:
  source $STAGING/tv-start-sshd.sh
EOF
